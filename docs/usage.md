# Using Hawk

Hawk analyzes public declarations in workspace library crates against
configured production targets and workspace non-production targets. This guide
covers invoking the tool; see [Configuration](configuration.md) for the
`hawk.toml` reference and [Architecture](architecture.md) for the analysis
model.

## Install a prebuilt release

Hawk is pinned to Rust 1.96.1 and uses `rustc_private`. A prebuilt release
still requires the exact normal Rust toolchain, but it does not require
`rustc-dev`, `RUSTC_BOOTSTRAP`, or a source build:

```sh
rustup toolchain install 1.96.1
```

Install the latest release with the standalone shell installer:

```sh
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/astral-sh/hawk/releases/latest/download/cargo-hawk-installer.sh | sh
```

The installer places `cargo-hawk` and `cargo-hawk-driver` on your `PATH` in
the same directory. You can instead download the archive for your platform
from [GitHub Releases](https://github.com/astral-sh/hawk/releases) and place
both executables on your `PATH` manually.

Run the Cargo subcommand with the pinned toolchain:

```sh
cargo +1.96.1 hawk --manifest-path /path/to/workspace/Cargo.toml
```

## Build Hawk

Hawk is pinned to Rust 1.96.1 and uses `rustc_private`; the repository
toolchain configuration installs `rustc-dev` when necessary. A source build
produces a `cargo-hawk` frontend and a `cargo-hawk-driver` compiler wrapper.

```sh
cargo build
```

## Configure production targets

Declare each shipped binary as a production target in `hawk.toml` at the root
of the workspace being analyzed:

```toml
[[production]]
package = "app"
bin = "app"
reason = "shipped application binary"
```

Every configured package and binary must be a target of that workspace. Hawk
does not infer production targets: an API used by an omitted binary can be
reported as unnecessary or dead. See [Configuration](configuration.md) for
multiple binaries, target-scoped entries, and accepted findings.

## Run analysis

```sh
./target/debug/cargo-hawk \
  --manifest-path /path/to/workspace/Cargo.toml
```

Configured production targets and workspace non-production targets are
analyzed under `--all-features --locked` on the host target by default. The
non-production surface includes tests, benches, examples, and compile-only
doctests. Diagnostics apply to workspace library crates compiled for those
targets, including declarations enabled only under `cfg(test)`.

Workspace libraries are treated as internal unless exempted. Exclude a
library crate whose public API is consumed outside the configured production
targets:

```sh
./target/debug/cargo-hawk \
  --manifest-path /path/to/workspace/Cargo.toml \
  --exclude-crate supported_library
```

Instrumented Cargo artifacts are reused under
`cargo-hawk-target/<workspace-name>` in the platform temporary directory by
default. Use `--target-dir` to override that location and `--graph-dir` to
retain serialized compiler fragments for investigation. Diagnostics are
colored automatically in a terminal; use `--color=always` or `--color=never`
to override terminal detection.

## Enforce diagnostics

Hawk reports diagnostics as warnings by default, so it can be introduced
without changing build status. Deny the `warnings` group to use Hawk as a CI
gate:

```sh
./target/debug/cargo-hawk \
  --manifest-path /path/to/workspace/Cargo.toml \
  -D warnings
```

Hawk accepts Clippy-style ordered `-A`/`--allow`, `-W`/`--warn`, and
`-D`/`--deny` lint levels. Later options take precedence:

```sh
./target/debug/cargo-hawk \
  --manifest-path /path/to/workspace/Cargo.toml \
  -D warnings \
  -W hawk::unnecessary_public
```

The supported selectors are `warnings`, `hawk::dead_public`,
`hawk::unnecessary_public`, `hawk::unnecessary_restricted_visibility`,
`hawk::unnecessary_crate_visibility`, `hawk::unknown_item`,
`hawk::ambiguous_item`, and `hawk::unfulfilled_expectation`. Denied diagnostics
are emitted as errors and cause a non-zero exit status. Invalid configuration
and failed instrumented Cargo builds fail independently of lint levels.

`hawk::unnecessary_crate_visibility` is allow-by-default because preferring
`pub(super)` over `pub(crate)` is a style choice. Enable it explicitly with
`-W hawk::unnecessary_crate_visibility` or
`-D hawk::unnecessary_crate_visibility`. The `warnings` group does not enable
allow-by-default lints.

## Apply fixes

Pass `--fix` to apply visibility reductions through Cargo's fix machinery:

```sh
./target/debug/cargo-hawk \
  --manifest-path /path/to/workspace/Cargo.toml \
  --fix
```

Hawk emits machine-applicable suggestions for enabled, unsuppressed
visibility findings. `hawk::unnecessary_public` reduces `pub` to `pub(crate)`.
`hawk::unnecessary_restricted_visibility` removes an explicit restricted
visibility modifier when the item can be private.
`hawk::unnecessary_crate_visibility` optionally reduces `pub(crate)` to
`pub(super)`. `hawk::dead_public` remains report-only because a
visibility-only edit can activate rustc's `dead_code` lint; removing dead
surface may require editing its remaining internal uses. Hawk delegates edit
application and validation to `cargo fix`, including Cargo's source-control
safety checks; pass `--allow-dirty`, `--allow-staged`, or `--allow-no-vcs`
with `--fix` when the corresponding Cargo override is appropriate.

Fixes are limited to workspace library packages in the configured production
or non-production surface. Hawk rechecks configured production targets and
non-production targets, including compile-only doctests, after applying
edits. Dead declarations and enum variants remain report-only.

## Analyze another target

Pass `--target TRIPLE` to analyze another compilation target. Hawk forwards
the target to Cargo but does not install a target SDK or configure a cross
linker.

For example, a macOS host can analyze Windows MSVC production targets using
[`cargo-xwin`](https://github.com/rust-cross/cargo-xwin). From the Hawk
checkout, prepare the pinned toolchain once:

```sh
rustup target add x86_64-pc-windows-msvc
rustup component add llvm-tools-preview
cargo install cargo-xwin --locked
```

Then export the linker and Windows SDK configuration for Hawk's child Cargo
process before running the analysis:

```sh
target=x86_64-pc-windows-msvc
eval "$(cargo xwin env --quiet \
  --target "$target" \
  --manifest-path /path/to/workspace/Cargo.toml)"

./target/debug/cargo-hawk \
  --manifest-path /path/to/workspace/Cargo.toml \
  --target "$target"
```

Target-scoped production entries and expectations can keep platform-specific
surfaces explicit; see [Configuration](configuration.md).
