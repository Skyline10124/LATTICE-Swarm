# Installation

Swarm is a Cargo workspace with two submodules.

## Requirements

- Rust toolchain
- Git with submodule support
- Provider API key for real model calls

## Clone

```sh
git clone --recurse-submodules git@github.com:Skyline10124/LATTICE-Swarm.git
```

If already cloned:

```sh
git submodule update --init --recursive
```

## Build Release Binary

```sh
cargo build --release -p lattice-cli
./target/release/lattice --help
```

## Config Directory

Swarm uses LATTICE Runtime profile discovery for agent profiles and local configuration. CLI session data is stored under the user's config directory.
