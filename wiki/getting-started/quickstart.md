# Quickstart

## Clone

```sh
git clone --recurse-submodules git@github.com:Skyline10124/LATTICE-Swarm.git
cd LATTICE-Swarm
```

## Configure Credentials

Set at least one provider key:

```sh
export DEEPSEEK_API_KEY="..."
export ANTHROPIC_API_KEY="..."
export OPENAI_API_KEY="..."
```

## Build

```sh
cargo build
```

## Run

```sh
cargo run -p lattice-cli -- run "1+1=?" -m deepseek-v4-flash
```

## Resolve a Model

```sh
cargo run -p lattice-cli -- resolve sonnet --trace
```

## Launch TUI

```sh
cargo run -p lattice-cli -- tui -m sonnet
```

## Run Tests

```sh
cargo test
```
