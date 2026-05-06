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

The TUI currently uses Runtime through the same lower crates used by CLI commands. The `lattice code` Module is the newer main-agent seam; the next UI pass should make the TUI an Adapter over that Module instead of duplicating agent construction.
