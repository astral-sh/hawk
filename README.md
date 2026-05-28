# hawk

[![CI](https://github.com/astral-sh/hawk/actions/workflows/ci.yml/badge.svg)](https://github.com/astral-sh/hawk/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)

A workspace-aware Cargo lint for unnecessary public Rust APIs.

**Experimental:** This project was authored by GPT-5.5 and is not intended
for public consumption. Use at your own risk.

Hawk finds `pub` declarations that are unused, or can be restricted to
`pub(crate)`, when a Cargo workspace builds one or more closed-world binary
products.

## Motivation

Hawk is intended for projects like [Ruff](https://github.com/astral-sh/ruff)
and [uv](https://github.com/astral-sh/uv), where the product is a binary but
the workspace itself is decomposed into many crates. In such projects, the
workspace is largely the only client of its `pub` APIs: `pub` is commonly
needed only to make symbols visible across crates within the workspace.

`rustc`'s `dead_code` lint identifies unused, unexported items, and its
opt-in `unreachable_pub` lint identifies `pub` items that cannot be reached
outside a single crate. It does not perform the closed-world analysis needed
to decide which workspace-internal `pub` APIs are actually required. Hawk
assumes that the workspace represents the world and identifies dead code and
unnecessarily public symbols across crates within a single workspace.

## Highlights

- Analyzes public surface across an entire Cargo workspace, starting from
  configured production targets.
- Reports `hawk::dead_public` for unused public items and
  `hawk::unnecessary_public` for visibility that can be restricted.
- Models production separately from tests, benches, examples, and doctests.
- Applies machine-applicable `pub(crate)` fixes through `cargo fix`.
- Uses Clippy-style `-A`/`-W`/`-D` lint levels for incremental CI adoption.

## Installation

Hawk uses `rustc_private` and must be compiled with its pinned Rust toolchain.
Install Rust 1.95.0 with the required compiler development component:

```sh
rustup toolchain install 1.95.0 --component rustc-dev
```

To install the current development version from Git:

```sh
RUSTC_BOOTSTRAP=1 cargo +1.95.0 install --locked \
  --git https://github.com/astral-sh/hawk cargo-hawk
```

Once Hawk is published on crates.io as `cargo-hawk`, install a released
version with:

```sh
RUSTC_BOOTSTRAP=1 cargo +1.95.0 install --locked cargo-hawk
```

`RUSTC_BOOTSTRAP=1` is required during installation because `cargo install`
does not use this repository's Cargo configuration when it compiles the
installed package.

## Getting started

Declare each shipped binary in a workspace-root `hawk.toml`:

```toml
[[production]]
package = "app"
bin = "app"
reason = "shipped application binary"
```

Analyze the workspace:

```sh
cargo hawk \
  --manifest-path /path/to/workspace/Cargo.toml
```

To enforce findings in CI or apply visibility fixes:

```sh
cargo hawk \
  --manifest-path /path/to/workspace/Cargo.toml \
  -D warnings

cargo hawk \
  --manifest-path /path/to/workspace/Cargo.toml \
  --fix
```

## Documentation

- [Using Hawk](docs/usage.md): running analysis, CI enforcement, fixes, and
  cross-compilation.
- [Configuration](docs/configuration.md): production targets, overrides,
  exclusions, and target selectors.
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

<div align="center">
  <a target="_blank" href="https://astral.sh" style="background:none">
    <img src="https://raw.githubusercontent.com/astral-sh/hawk/main/assets/svg/Astral.svg" alt="Made by Astral">
  </a>
</div>
