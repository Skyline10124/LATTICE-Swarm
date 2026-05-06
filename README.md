# LATTICE-Swarm

![LATTICE banner](logo-banner.svg)

Swarm is the user-facing LATTICE repository. It owns the CLI and TUI, and embeds the two implementation repositories as submodules:

- [LATTICE-Runtime](https://github.com/Skyline10124/LATTICE-Runtime): model routing, transports, agent loop, plugin runtime, bus and Python binding.
- [LATTICE-Plugins](https://github.com/Skyline10124/LATTICE-Plugins): official typed plugins.

The legacy mono-repo at `~/lattice` is no longer maintained. Primary maintenance happens in Runtime and Swarm.

## Clone

```sh
git clone --recurse-submodules git@github.com:Skyline10124/LATTICE-Swarm.git
cd LATTICE-Swarm
```

If the repository was cloned without submodules:

```sh
git submodule update --init --recursive
```

## Workspace

```text
LATTICE-Swarm/
├── lattice-cli/       CLI and ratatui TUI
├── LATTICE-Runtime/   submodule: runtime crates
└── LATTICE-Plugins/   submodule: official plugins
```

Swarm uses local path dependencies into the submodules. The root `Cargo.toml` patches Runtime git dependencies so `LATTICE-Plugins` and `lattice-cli` use the same local Runtime instance during submodule development.

## Build

```sh
cargo build
cargo test
cargo clippy -- -D warnings
cargo fmt --all --check
```

## CLI

```sh
cargo run -p lattice-cli -- run "1+1=?" -m deepseek-v4-flash
cargo run -p lattice-cli -- resolve sonnet --trace
cargo run -p lattice-cli -- models --auth
cargo run -p lattice-cli -- tui -m sonnet
```

Credentials are read from environment variables or config:

```sh
export ANTHROPIC_API_KEY="..."
export OPENAI_API_KEY="..."
export DEEPSEEK_API_KEY="..."
```

## Submodule Workflow

Update submodules to their latest main branches:

```sh
git submodule update --remote --merge LATTICE-Runtime
git submodule update --remote --merge LATTICE-Plugins
cargo test
git add LATTICE-Runtime LATTICE-Plugins Cargo.lock
git commit -m "Update runtime and plugin submodules"
```

Work directly in a submodule when changing Runtime or Plugins:

```sh
cd LATTICE-Runtime
# edit, test, commit, push
cd ..
git add LATTICE-Runtime
git commit -m "Update runtime submodule"
```

## Documentation

Start with [wiki/README.md](wiki/README.md).

Key pages:

- [Getting Started](wiki/getting-started/quickstart.md)
- [Submodule Workflow](wiki/development/submodules.md)
- [CLI Reference](wiki/reference/cli.md)
- [TUI Notes](wiki/reference/tui.md)
- [Operations](wiki/operations/runtime-layout.md)

## License

Apache-2.0. See [LICENSE](LICENSE).
