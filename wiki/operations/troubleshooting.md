# Troubleshooting

## Missing Submodules

Error symptoms include missing `LATTICE-Runtime` or `LATTICE-Plugins` paths.

Fix:

```sh
git submodule update --init --recursive
```

## Duplicate Runtime Types

If Rust reports mismatched Runtime types such as `PluginRegistry`, check that both `lattice-cli` and `LATTICE-Plugins` depend on the same local `LATTICE-Runtime/lattice` crate.

## Model Resolution Fails

Run:

```sh
cargo run -p lattice-cli -- doctor
cargo run -p lattice-cli -- resolve sonnet --trace
```

Check provider environment variables and base URL configuration.

## Plugin Not Found

Official plugins come from the `LATTICE-Plugins` submodule. Confirm the submodule is initialized and that the feature is enabled in `lattice-plugins`.
