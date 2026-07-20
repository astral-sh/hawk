# hawk

[![CI](https://github.com/astral-sh/hawk/actions/workflows/ci.yml/badge.svg)](https://github.com/astral-sh/hawk/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/cargo-hawk.svg)](https://crates.io/crates/cargo-hawk)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)

A workspace-aware Cargo lint for unnecessary Rust visibility.

**Experimental:** This project was authored by [Codex](https://openai.com/codex/) and is not intended
for public consumption. Use at your own risk.

Hawk finds `pub` declarations that are unused or can be restricted to
`pub(crate)` when a Cargo workspace builds one or more shipped binaries. It
also finds explicit restricted visibility modifiers that can be removed.
Optionally, it can suggest restricting `pub(crate)` declarations to
`pub(super)`.

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
- Reports `hawk::dead_public` for unused public items,
  `hawk::unnecessary_public` for `pub` items that can become `pub(crate)`, and
  `hawk::unnecessary_restricted_visibility` for restricted items that can
  become private.
- Optionally reports `hawk::unnecessary_crate_visibility` for `pub(crate)`
  items that can become `pub(super)`.
- Models production separately from tests, benches, examples, and doctests.
- Applies machine-applicable visibility fixes through `cargo fix`.
- Uses Clippy-style `-A`/`-W`/`-D` lint levels for incremental CI adoption.

## Installation

Hawk uses `rustc_private` and must run with the exact Rust toolchain it was
built against. Prebuilt releases are available for macOS and Linux, but they
are not independent of Rust. Install the normal Rust 1.97.0 toolchain:

```sh
rustup toolchain install 1.97.0
```

Install the latest prebuilt release:

```sh
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/astral-sh/hawk/releases/latest/download/cargo-hawk-installer.sh | sh
```

The installer places `cargo-hawk` and its internal `cargo-hawk-driver`
executable together on your `PATH`. You can instead download the archive for
your platform from [GitHub Releases](https://github.com/astral-sh/hawk/releases)
and place both executables on your `PATH` in the same directory. A prebuilt
release does not require `rustc-dev`, `RUSTC_BOOTSTRAP`, or a source build.

Hawk validates the selected compiler before analysis. If a workspace selects
another Rust version, invoke Hawk with its pinned toolchain:

```sh
cargo +1.97.0 hawk check
```

To build Hawk from source, install the compiler development component:

```sh
rustup toolchain install 1.97.0 --component rustc-dev
```

To install the current development version from Git:

```sh
RUSTC_BOOTSTRAP=1 cargo +1.97.0 install --locked \
  --git https://github.com/astral-sh/hawk cargo-hawk
```

Install a released version from crates.io with:

```sh
RUSTC_BOOTSTRAP=1 cargo +1.97.0 install --locked cargo-hawk
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

Run `cargo hawk` to see the available commands. Analyze the workspace with
`check`:

```sh
cargo hawk check \
  --manifest-path /path/to/workspace/Cargo.toml
```

To enforce findings in CI or apply visibility fixes:

```sh
cargo hawk check \
  --manifest-path /path/to/workspace/Cargo.toml \
  -D warnings

cargo hawk check \
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
Because it integrates with compiler internals, it is pinned to Rust 1.97.0.
Hawk was authored entirely by [Codex](https://openai.com/codex/).

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
