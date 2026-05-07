# Submodule Workflow

Swarm contains Runtime and Plugins as submodules.

```text
LATTICE-Swarm/
├── LATTICE-Runtime
└── LATTICE-Plugins
```

## Update Submodules

```sh
git submodule update --remote --merge LATTICE-Runtime
git submodule update --remote --merge LATTICE-Plugins
cargo test
git add LATTICE-Runtime LATTICE-Plugins Cargo.lock
git commit -m "Update runtime and plugin submodules"
git push
```

## Work in Runtime

```sh
cd LATTICE-Runtime
cargo test
git add .
git commit -m "Change runtime behavior"
git push

cd ..
git add LATTICE-Runtime Cargo.lock
git commit -m "Update runtime submodule"
git push
```

## Work in Plugins

```sh
cd LATTICE-Plugins
cargo test
git add .
git commit -m "Change official plugins"
git push

cd ..
git add LATTICE-Plugins Cargo.lock
git commit -m "Update plugins submodule"
git push
```

## Local Runtime Dependency

Swarm uses local path dependencies to `LATTICE-Runtime/lattice`. This prevents two copies of Runtime from being compiled when `LATTICE-Plugins` is built inside Swarm.
