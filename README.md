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
retain the compiler fragments in a run-specific subdirectory for investigation.
Diagnostics are colored automatically in a terminal; use `--color=always` or
`--color=never` to override terminal detection.

## Configuration

Add `hawk.toml` at the workspace root to suppress an intentional finding or
pin one as an expected finding:

```toml
[[override]]
lint = "hawk::dead_public"
crate = "library"
item = "legacy_entry"
level = "allow"
reason = "retained temporarily while consumers migrate"

[[override]]
lint = "hawk::unnecessary_public"
crate = "library"
item = "generated_registration"
level = "expect"
reason = "called by generated registration that Hawk does not model"
```

`allow` suppresses a matching finding. `expect` suppresses a matching finding
and reports `hawk::unfulfilled_expectation` if that exact finding is no longer
present. An entry whose `crate` and `item` selector no longer identifies a
compiled item reports `hawk::unknown_item`.

Overrides filter diagnostics only; they do not add reachability roots or
preserve visibility for referenced items. Use `--config PATH` to load a
configuration file other than the workspace-root `hawk.toml`.

## License

hawk is licensed under either of

- Apache License, Version 2.0, ([LICENSE-APACHE](LICENSE-APACHE) or
  <https://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or <https://opensource.org/licenses/MIT>)

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in
hawk by you, as defined in the Apache-2.0 license, shall be dually licensed as above, without any
additional terms or conditions.
