# LATTICE-Swarm

User-facing LATTICE entrypoint.

This repository owns the CLI and TUI surfaces. Runtime execution, bus, model transport, Python bindings and plugin contracts live in the `LATTICE-Runtime` submodule. Official plugin implementations live in the `LATTICE-Plugins` submodule.

Clone with submodules:

```sh
git clone --recurse-submodules git@github.com:Skyline10124/LATTICE-Swarm.git
```

Update submodules:

```sh
git submodule update --init --recursive
```

Primary maintenance happens in `LATTICE-Runtime` and `LATTICE-Swarm`. The legacy mono-repo at `~/lattice` is no longer maintained.

License: Apache-2.0.
