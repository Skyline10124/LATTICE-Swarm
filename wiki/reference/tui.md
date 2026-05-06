# TUI Reference

The TUI lives under `lattice-cli/src/frontend/tui`.

It provides:

- streaming chat display
- markdown rendering
- session loading and persistence
- model switching
- slash suggestions
- tool output display
- status line with runtime state

Run:

```sh
cargo run -p lattice-cli -- tui -m sonnet
```

The TUI uses Runtime through the same `lattice-agent`, `lattice-bus` and `lattice-core` crates used by CLI commands.
