# hawk

`hawk` is an experimental Cargo lint tool for binary products built from
internal Rust workspace crates. It analyzes public library items in a selected
binary product against its production binary and workspace test consumers,
reporting items that are unused or whose visibility exceeds those consumers'
needs.

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

The selected binary and workspace test targets are analyzed under
`--all-features --locked` on the host target by default. Pass `--target TRIPLE`
to analyze another compilation target; Hawk expects any required
cross-compilation environment to be prepared by the caller. Diagnostics apply
only to workspace library crates compiled for the selected binary; workspace
tests participate as consumers of those crates. Those libraries are considered
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

By default, Hawk reports diagnostics as warnings and exits successfully so it
can be introduced without changing build status. To use it as a CI gate, deny
the `warnings` group:

```sh
./target/debug/cargo-hawk \
  --manifest-path /path/to/workspace/Cargo.toml \
  --package app \
  --bin app \
  -D warnings
```

Hawk accepts Clippy-style `-A`/`--allow`, `-W`/`--warn`, and `-D`/`--deny`
lint levels. Later options take precedence, so CI can enforce most diagnostics
while introducing one incrementally:

```sh
./target/debug/cargo-hawk \
  --manifest-path /path/to/workspace/Cargo.toml \
  --package app \
  --bin app \
  -D warnings \
  -W hawk::unnecessary_public
```

The supported selectors are `warnings`,
`hawk::dead_public`, `hawk::unnecessary_public`,
`hawk::unknown_item`, and `hawk::unfulfilled_expectation`. Denied
diagnostics are printed as errors and cause a non-zero exit status. Invalid
configuration or a failed instrumented Cargo build fails regardless of lint
levels.

## Fixes

Pass `--fix` to apply visibility reductions through Cargo's `fix` machinery:

```sh
./target/debug/cargo-hawk \
  --manifest-path /path/to/workspace/Cargo.toml \
  --package app \
  --bin app \
  --fix
```

Hawk emits machine-applicable `pub` to `pub(crate)` suggestions for findings
that are not suppressed and are enabled at the command line. It delegates edit
application and validation to `cargo fix`, including Cargo's source-control
safety checks; pass `--allow-dirty`, `--allow-staged`, or `--allow-no-vcs` with
`--fix` when the corresponding Cargo override is appropriate.

Unlike `cargo clippy --fix`, Hawk applies fixes only to library packages in the
selected production product. Production findings are fixed through library
targets, while declarations needed only by tests are fixed through their
owning packages' library and test targets. This covers production declarations
in test-support packages even when their library test harness is disabled.
Declarations compiled only under `cfg(test)` are not diagnostic candidates
yet. Hawk caps ordinary compiler lints during the fix phase so Cargo applies
Hawk's planned suggestions rather than unrelated compiler fixes, then rechecks
the selected binary and workspace tests. Enum variants are report-only because
they have no independent visibility modifier; a variant finding disappears
after fixing its containing enum only when the entire enum no longer needs to
be public.

## Cross-compilation

Hawk forwards `--target` to Cargo, but it does not install a target SDK or
configure a cross linker. For example, a macOS host can analyze a Windows
MSVC product using [`cargo-xwin`](https://github.com/rust-cross/cargo-xwin).
From the Hawk checkout, prepare its pinned Rust toolchain once:

```sh
rustup target add x86_64-pc-windows-msvc
rustup component add llvm-tools-preview
cargo install cargo-xwin --locked
```

Then export the linker and Windows SDK configuration for the child Cargo
process before running Hawk:

```sh
target=x86_64-pc-windows-msvc
eval "$(cargo xwin env --quiet \
  --target "$target" \
  --manifest-path /path/to/workspace/Cargo.toml)"

./target/debug/cargo-hawk \
  --manifest-path /path/to/workspace/Cargo.toml \
  --package app \
  --bin app \
  --target "$target"
```

Platform-specific expectations can be scoped in `hawk.toml`, for example with
`target = "cfg(windows)"` or `target = "cfg(not(windows))"`, so a run on one
platform does not validate an item that is only compiled on another.

## Coverage

Hawk diagnoses public free functions, inherent methods and associated
constants, traits, named types (including unions and type aliases), constants,
statics, struct and union fields, and enum variants. Field construction,
projection, patterns, and `offset_of!` uses, as well as enum variant
construction and matching, participate in reachability and cross-crate
visibility analysis.

For fields and inherent associated constants, a live item used only inside its
defining crate can be changed to `pub(crate)`. Enum variants have no
independent Rust visibility modifier: Hawk diagnoses unreachable variants for
removal, but does not report reachable variants as unnecessary public surface.

## Test Consumers

Hawk keeps production and test reachability distinct. An item referenced
across crate boundaries by a workspace test must remain `pub` and is not
reported. A public helper reachable only along test paths, without a
cross-crate use of its own, is reported as `hawk::unnecessary_public`, with a
`pub(crate)` suggestion. A public declaration unreachable from both the
selected binary and workspace tests remains `hawk::dead_public`.

## Exported paths and modules

In addition to public declarations, Hawk diagnoses selected public re-exports
and public modules. A named local re-export of a modeled non-module
declaration is reported only when no compiled cross-crate reference could
require that exported path. If its target is not reachable from the selected
binary it is dead public surface; if its target is used only without a
required external path, the re-export can be restricted to `pub(crate) use`.

Public module visibility is tracked through declarations lexically nested
inside the module. A cross-crate reference to a descendant conservatively
preserves its public module ancestors; a module whose reachable descendants
are internal can be restricted to `pub(crate) mod`.

Rustc resolves consumer paths through `pub use` to the underlying declaration,
so the graph cannot identify which alias was used. To avoid suggestions that
could fail privacy checking, Hawk does not report glob re-exports, re-exports
of modules or unmodeled/external targets, and it preserves a public module
that contains a public re-export. These are intentional false negatives until
export-path provenance is available.

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

[[override]]
lint = "hawk::dead_public"
crate = "platform"
item = "windows_only_api"
level = "expect"
target = "cfg(windows)"
reason = "public API retained only in the Windows build"
```

`allow` suppresses a matching finding. `expect` suppresses a matching finding
and reports `hawk::unfulfilled_expectation` if that exact finding is no longer
present. An entry whose `crate` and `item` selector no longer identifies a
compiled item reports `hawk::unknown_item`. An optional `target` accepts the
same named targets and `cfg(...)` platform expressions as Cargo target
dependencies; the override is checked only while analyzing a matching target.
For newly analyzed paths, `item` uses the exported alias name (for example
`PublicAlias`) or module path (for example `api::internal`).

Overrides filter diagnostics only; they do not add reachability roots or
preserve visibility for referenced items. Use `--config PATH` to load a
configuration file other than the workspace-root `hawk.toml`.
With `-D warnings`, correctly suppressed diagnostics do not fail the command,
while stale selectors and unfulfilled expectations do unless lowered or
allowed explicitly.

## License

hawk is licensed under either of

- Apache License, Version 2.0, ([LICENSE-APACHE](LICENSE-APACHE) or
  <https://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or <https://opensource.org/licenses/MIT>)

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in
hawk by you, as defined in the Apache-2.0 license, shall be dually licensed as above, without any
additional terms or conditions.
