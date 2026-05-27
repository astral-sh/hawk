# hawk

[![CI](https://github.com/astral-sh/hawk/actions/workflows/ci.yml/badge.svg)](https://github.com/astral-sh/hawk/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)

A workspace-aware Cargo lint for unnecessary public Rust APIs.

Hawk finds `pub` declarations that are unused, or can be restricted to
`pub(crate)`, when a Cargo workspace builds one or more closed-world binary
products.

## Highlights

- Analyzes public surface across an entire Cargo workspace, starting from
  configured production binaries.
- Reports `hawk::dead_public` for unused public items and
  `hawk::unnecessary_public` for visibility that can be restricted.
- Models production separately from tests, benches, examples, and doctests.
- Applies machine-applicable `pub(crate)` fixes through `cargo fix`.
- Uses Clippy-style `-A`/`-W`/`-D` lint levels for incremental CI adoption.

## Getting started

Hawk currently requires its pinned Rust toolchain and uses `rustc_private`.
Declare each shipped binary in a workspace-root `hawk.toml`:

```toml
[[production]]
package = "app"
bin = "app"
reason = "shipped application binary"
```

Build Hawk and analyze the workspace:

```sh
cargo build
./target/debug/cargo-hawk \
  --manifest-path /path/to/workspace/Cargo.toml
```

To enforce findings in CI or apply visibility fixes:

```sh
./target/debug/cargo-hawk \
  --manifest-path /path/to/workspace/Cargo.toml \
  -D warnings

./target/debug/cargo-hawk \
  --manifest-path /path/to/workspace/Cargo.toml \
  --fix
```

## Documentation

- [Using Hawk](docs/usage.md): running analysis, CI enforcement, fixes, and
  cross-compilation.
- [Configuration](docs/configuration.md): product binaries, overrides, and
  target selectors.
- [Architecture](docs/architecture.md): how Hawk differs from Clippy and how
  the workspace analysis is implemented.
- [MVP design](docs/mvp-design.md): the original analysis scope and design
  rationale.

## Status

Hawk is experimental. It assumes workspace library crates are internal to the
configured binary product unless they are explicitly excluded from analysis.
Because it integrates with compiler internals, it is pinned to Rust 1.95.0.
Hawk was authored entirely by GPT-5.5 in Codex.

## License

hawk is licensed under either of

- Apache License, Version 2.0, ([LICENSE-APACHE](LICENSE-APACHE) or
  <https://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or <https://opensource.org/licenses/MIT>)

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in
hawk by you, as defined in the Apache-2.0 license, shall be dually licensed as above, without any
additional terms or conditions.
