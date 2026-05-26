# hawk

`hawk` is an experimental Cargo lint tool for binary products built from
internal Rust workspace crates. It analyzes a selected binary as a closed
world and reports public library items that are not needed by that product or
whose visibility exceeds the needs of the product.

This repository is at the prototype stage.

## Usage

`hawk` is pinned to Rust 1.95.0 and uses `rustc_private`; the repository
toolchain configuration installs `rustc-dev` when necessary.
The build embeds the selected compiler sysroot runtime path so the resulting
executable can run directly as Cargo's compiler wrapper.

```sh
cargo build
./target/debug/cargo-hawk \
  --manifest-path /path/to/workspace/Cargo.toml \
  --package app \
  --bin app
```

The selected binary is analyzed under `--all-features --locked` on the host
target. All workspace library crates compiled for that binary are considered
internal unless exempted:

```sh
./target/debug/cargo-hawk \
  --manifest-path /path/to/workspace/Cargo.toml \
  --package app \
  --bin app \
  --exclude-crate supported_library
```

Instrumented Cargo artifacts are reused under `/private/tmp/codex-hawk-target`
by default. Use `--target-dir` to override that location and `--graph-dir` to
retain the compiler fragments for investigation.

## License

hawk is licensed under either of

- Apache License, Version 2.0, ([LICENSE-APACHE](LICENSE-APACHE) or
  <https://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or <https://opensource.org/licenses/MIT>)

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in
hawk by you, as defined in the Apache-2.0 license, shall be dually licensed as above, without any
additional terms or conditions.
