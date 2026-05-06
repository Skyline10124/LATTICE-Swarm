# TUI Reference

The TUI lives under `lattice-cli/src/frontend/tui`.

It provides:

- streaming chat display
- markdown rendering
- session loading and persistence
- model switching
- slash suggestions
- slash command completion with `Tab`
- queued prompts while a response is streaming
- transcript search with `/find`, `/next`, and `/prev`
- tool call cards with running/done/error state, arguments, elapsed time, and collapsed result previews
- per-message thinking blocks, collapsed by default with `Ctrl+O` to expand the latest block and `/trace` to show reasoning inline
- Runtime sandbox status in the status line
- session-local permission switching with `/permissions <project|strict|permissive|off>`
- Runtime plugin discovery with `/plugins`
- one-shot plugin runs with `/plugin <name> <prompt>`
- status line with runtime state

Run:

```sh
cargo run -p lattice-cli -- tui --model sonnet
```

Local manifest plugins can be added to TUI discovery with:

```sh
cargo run -p lattice-cli -- tui --plugins-dir ./plugins
```

The TUI currently uses Runtime through the same lower crates used by CLI commands. The `lattice code` Module is the newer main-agent seam; the next UI pass should make the TUI an Adapter over that Module instead of duplicating agent construction.
