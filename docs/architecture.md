# Architecture

Hawk applies a Clippy-shaped user experience to a question that cannot be
answered by a single crate compilation: which public declarations are
actually needed by a closed-world Cargo workspace product?

A binary product such as `uv` or `ruff` may be split across many internal
library crates. Rust requires cross-crate references to cross a `pub`
boundary, even when no external library API is intended. Rustc and Clippy see
each of those crate boundaries; Hawk additionally knows which workspace
binaries constitute the production targets and which workspace targets consume
code only outside production.

This document describes the implementation on `main`. It compares Hawk with
Clippy's architecture and tooling model, not with the implementation of any
one Clippy lint.

## Comparison with Clippy

| Concern                       | Clippy                                                                                        | Hawk                                                                                             |
| ----------------------------- | --------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------ |
| User entry point              | `cargo clippy`, or `clippy-driver` directly                                                   | `cargo hawk check`; `cargo-hawk-driver` is an internal executable                                |
| Cargo integration             | Sets `RUSTC_WORKSPACE_WRAPPER=clippy-driver`                                                  | Sets `RUSTC_WORKSPACE_WRAPPER=cargo-hawk-driver`                                                 |
| Compiler integration          | A `rustc_driver` callback registers Clippy lint passes with rustc's lint store                | A `rustc_driver` callback inspects analyzed HIR/type context and serializes graph fragments      |
| Unit of analysis              | One rustc invocation at a time                                                                | All instrumented crate compilations from selected production targets and non-production surface  |
| When diagnostics are decided  | During the compiler lint pass that finds the condition                                        | After Cargo completes, when graph fragments have been merged and traversed                       |
| Meaning of a public item      | Rust/Clippy lint semantics within the compilation being checked                               | A declaration that may be unnecessary under an explicit closed-world product model               |
| Suppression and severity      | Rust lint attributes and command-line lint levels, plus Clippy configuration where applicable | Clippy-style command-line levels plus reasoned `hawk.toml` overrides and scoped exclusions       |
| Automatic fixes               | The lint emits suggestions; `cargo clippy --fix` delegates application to `cargo fix`         | Graph analysis first writes a fix plan; a second compiler pass emits suggestions for `cargo fix` |
| Compiler-coupled distribution | Ships with a matching Rust toolchain as a rustup component                                    | Release archives require the exact matching normal Rust toolchain                                |

The shared foundation is deliberate. Both tools run as Cargo compiler
wrappers, use `rustc_private` compiler APIs, produce lint-style diagnostics,
and rely on Cargo's fix machinery for machine-applicable edits. Consequently,
both are coupled to a particular Rust compiler toolchain.

The architectural split is equally deliberate. A normal Clippy lint can
observe everything it needs during the crate currently being compiled. Hawk
must distinguish:

- an internal `pub` item reached from a selected production target;
- an item needed across a crate boundary and therefore required to remain
  `pub`;
- an item used only by tests;
- an item never reached by any relevant consumer.

Those facts can originate in different Cargo target builds and different
crate compilations. Hawk therefore uses rustc as a fact collector and runs
its lint decision as a workspace-level post-processing step.

## Distribution and compiler coupling

Clippy and Hawk are both coupled to a particular compiler because their
drivers link Rust compiler internals. Clippy is distributed as a rustup
component alongside its matching compiler. Hawk is not a rustup component, so
cargo-dist packages each prebuilt release with two executables:

- `cargo-hawk` is the user-facing frontend and does not link Rust compiler
  libraries;
- `cargo-hawk-driver` is the internal compiler wrapper and dynamically links
  `librustc_driver`.

The archive and shell installer keep the executables in the same directory.
Before starting analysis, the frontend verifies that the selected `rustc`
release, commit hash, and host triple match the compiler used to build Hawk.
It then supplies that compiler sysroot's driver-library directory when Cargo
launches the wrapper. For the supported release targets, the normal Rust
toolchain provides the runtime compiler library; `rustc-dev` is needed only
to build Hawk itself.

This makes a release archive portable between machines with the same host
triple and exact Rust toolchain. It does not make Hawk independent of Rust or
compatible with arbitrary compiler versions.

## Product model

Hawk treats workspace library crates as internal implementation crates unless
the caller excludes them with `--exclude-crate`. It does not infer production
targets from every binary that happens to compile. Each production target is
stated in `hawk.toml`:

```toml
[[production]]
package = "uv"
bin = "uv"
reason = "shipped package manager binary"
```

Applicable `[[production]]` entries seed the production graph. Hawk also
compiles workspace non-production targets under
`cargo check --workspace --all-targets` and compile-only doctests under
`cargo test --workspace --doc`. Configured `[[doctest]]` entries replace the
workspace selection with explicit packages:

- executable non-production targets, including test harnesses and
  `harness = false` benchmarks or tests, seed a second reachability graph;
- tests, benches, examples, and doctests preserve public visibility whenever
  a compiled cross-crate reference requires it;
- test-only compilation can expose `#[cfg(test)]` declarations and
  dev-dependency support crates as diagnostic candidates.

Instrumented builds use the configured feature profiles and one selected
target triple. With no explicit profiles, Hawk uses one `--all-features`
profile. A `target = "cfg(...)"` selector on a production entry, override, or
exclusion limits it to applicable target configurations.

Workspace library target names must be unique after Rust crate-name
normalization. Hawk uses the crate name to associate compiler fragments,
configuration, and fix targets with Cargo packages, and rejects a workspace
where that association would be ambiguous.

This is the central semantic difference from Clippy. Running Clippy over a
workspace changes which crates are checked, but it does not define the
workspace as the complete external consumer of its internal libraries. Hawk's
configured production-target model does.

## Execution pipeline

`cargo-hawk` is the frontend executable. It locates its sibling
`cargo-hawk-driver`, validates the selected compiler, and configures Cargo to
use the driver as its compiler wrapper.

An analysis run proceeds as follows:

```text
 cargo-hawk check
     |
     | read Cargo metadata, hawk.toml, lint levels, and target cfg
     v
 cargo check --package <package> --bin <target>    (once per target and feature profile)
 cargo check --workspace --all-targets              (once per feature profile)
 cargo test --workspace|--package <package> --doc   (once per feature profile)
     |
     | RUSTC_WORKSPACE_WRAPPER=cargo-hawk-driver
     v
 cargo-hawk-driver rustc_driver callback after_analysis
     |
     | one JSON Fragment per compiled workspace crate
     v
 graph::analyze(all production fragments, all non-production fragments)
     |
     | apply hawk.toml suppressions and command-line levels
     v
 rustc-shaped Hawk diagnostics
```

Every Cargo invocation includes the selected profile's feature arguments,
`--locked`, a shared target directory, and optional `--target`. Each profile
writes to separate production and non-production fragment directories. The
frontend combines those fragments before graph analysis. Environment variables
identify the fragment output directory, selected production-target root,
analysis mode, and run ID.
For doctests, Hawk additionally uses rustdoc's test-builder wrapper to route
the generated test crates through the compiler wrapper without executing
them. The run ID is tracked as compiler dependency input so Cargo does not
reuse a prior instrumented compilation without producing fresh fragments.

Clippy follows the first half of this shape: its Cargo frontend invokes Cargo
with `RUSTC_WORKSPACE_WRAPPER=clippy-driver`, and `clippy-driver` calls
`rustc_driver::run_compiler`. The difference is that Clippy's driver registers
lint passes in `config.register_lints`; those passes emit diagnostics as each
crate is checked. Hawk's callback runs after rustc analysis and emits data,
not findings, during the collection phase.

## Compiler fragments

The wrapper records a `Fragment` for each compiled workspace crate. A fragment
contains:

- the owning Cargo package name and rustc crate identity, which are tracked
  separately;
- definitions, including source location, item kind, lexical module scope,
  and whether the item is a public-surface, restricted-visibility, or
  crate-visible candidate;
- typed reference edges extracted from bodies and public interfaces;
- entry-point roots when the crate is a selected production target or non-production
  executable;
- conservative roots for code whose indirect execution cannot be safely
  recovered from direct call edges;
- required-public roots for visibility constraints that are independent of
  runtime reachability.

The public item coverage currently includes free functions, inherent methods
and associated constants, traits, named types, constants, statics, struct and
union fields, enum variants, named local re-exports, and public modules.
Macro-expanded declarations are not direct candidates.

For production compilation, public candidates must be exported according to
rustc's effective visibility information. For a test-harness compilation,
Hawk also admits locally public declarations so it can analyze APIs compiled
only for tests without broadening the production surface. Non-production
executables that are not test harnesses still contribute liveness roots, but
do not expand this test-only candidate surface. Explicit restricted visibility
modifiers are tracked separately so their compiled uses can be compared with
their lexical module scope. Exact `pub(crate)` declarations are also tracked
for the optional `pub(super)` reduction.

### Edge kinds

`src/bin/driver/mod.rs` produces five kinds of graph edge:

| Edge                    | Purpose                                                                                 |
| ----------------------- | --------------------------------------------------------------------------------------- |
| `Body`                  | A value, function, field, or variant is used by executable code.                        |
| `Interface`             | A declaration exposes another declaration in its type or ownership relationship.        |
| `Reexport`              | A named public `use` targets another declaration.                                       |
| `VisibilityParent`      | A public item is nested below a module whose visibility may also be required.           |
| `VisibilityRequirement` | A visibility relationship must be preserved even though it is not runtime reachability. |

Separating body reachability from visibility requirements is essential. An
item can be absent from production execution while still requiring `pub` because
some compiled cross-crate signature, re-export, or generated interface relies
on its visibility.

## Graph analysis

`src/graph.rs` merges the fragments from all compiled graphs before emitting a
finding. The same source declaration may have separate compiler identities
when built for production and for non-production targets. Hawk deliberately
merges identities by physical source span and item kind, including a path
module compiled into different crates; declarations without source spans use
their diagnostic paths instead.

The analysis then computes two reachability closures:

- **production live** begins at each configured production target entry point;
- **non-production live** begins at executable entry points compiled for
  tests, benches, examples, or doctests.

Both closures include conservative roots, currently used for trait-associated
implementation code whose dispatch is not safely modeled by direct call
edges.

Separately, Hawk computes the declarations whose public visibility is
required. Any compiled cross-crate reference requires the referenced
declaration to retain visibility, regardless of whether the referencing item
is reachable from a selected root: rustc privacy-checks compiled code, not
only production runtime code. The requirement propagates along interface,
re-export, visibility-parent, and explicit visibility-requirement edges.

For each public candidate in a non-excluded workspace library crate:

| State                                                             | Result                     |
| ----------------------------------------------------------------- | -------------------------- |
| Not live in production or non-production, and not required public | `hawk::dead_public`        |
| Live in production or non-production, but not required public     | `hawk::unnecessary_public` |
| Required public by a compiled cross-crate consumer or interface   | no visibility finding      |

A selected production target is not a library surface to reduce, so its crate
does not receive these findings.

For each explicit restricted-visibility candidate, Hawk separately finds every
compiled reference across production and non-production fragments:

| State                                              | Result                                                     |
| -------------------------------------------------- | ---------------------------------------------------------- |
| Every compiled use fits within the defining module | `hawk::unnecessary_restricted_visibility`: suggest private |
| A compiled use requires a broader scope            | no private-visibility finding                              |

For an exact `pub(crate)` candidate that cannot become private, Hawk can
additionally suggest `pub(super)` when every compiled use fits within the
defining module's parent. This preference is reported as
`hawk::unnecessary_crate_visibility` and is allow-by-default.

With `preserve-uniform-field-visibility = true`, Hawk also treats a complete,
source-written struct or union field list as a visibility group when every
field has the same written visibility. If one member semantically requires the
group's current visibility, reducible visibility findings for its siblings are
omitted. Dead-public findings remain independent, and overrides, exclusions,
and lint levels do not establish a visibility requirement.

Enum variants are a special case. Hawk can report an unreachable public
variant as dead surface requiring removal together with any remaining
unreachable uses, but it does not report a reachable variant as unnecessarily
public because Rust does not provide an independent `pub(crate)` modifier for
a variant.

## Conservative boundaries

Like Clippy, Hawk should avoid fixes that change valid code into code that no
longer compiles. Workspace-global analysis adds several privacy-specific
boundaries:

- A compiled cross-crate use preserves `pub` even if the use is outside
  production reachability.
- Named local re-exports are analyzed only where the target can be modeled
  soundly. Glob re-exports, module re-exports, and unmodeled or external
  targets remain conservative false negatives.
- Crate-visible re-exports are not reduced because rustc does not preserve
  enough path provenance to prove which exported path a compiled use needs.
- Modules containing explicitly visible re-exports are not reduced because
  rustc resolves consumer paths directly to the re-export target.
- Public module ancestors are retained when an externally required
  declaration could be addressed through that lexical path.
- Public trait implementation interfaces can require exposed types to remain
  public, even without a direct call path.
- Proc-macro exports are required public because their attributed entry points
  are constrained by rustc.
- Derive-expanded field interfaces preserve matching source-field visibility
  where generated exposure cannot otherwise be proven from HIR.

These choices are not additional public API promises. They represent cases
where Hawk does not yet have enough provenance to recommend a compiling
visibility reduction.

## Diagnostics and configuration

Hawk uses Clippy-style ordered command-line levels:

```sh
cargo-hawk check --manifest-path Cargo.toml -D warnings -W hawk::unnecessary_public
```

The visibility diagnostics are `hawk::dead_public`, `hawk::unnecessary_public`,
`hawk::unnecessary_restricted_visibility`, and
`hawk::unnecessary_crate_visibility`. The final lint is allow-by-default;
enable it explicitly to prefer `pub(super)` over `pub(crate)`. Configuration
validation adds `hawk::unknown_item`, `hawk::ambiguous_item`, and
`hawk::unfulfilled_expectation`.

Hawk's workspace-level decisions do not naturally map to source attributes in
a single crate compilation. Instead, `hawk.toml` carries documented
exceptions:

```toml
[[override]]
lint = "hawk::unnecessary_public"
crate = "library"
item = "generated_registration"
kind = "function"
level = "expect"
reason = "called by generated registration that Hawk does not model"
```

`allow` suppresses the selected finding. `expect` suppresses it while
producing a diagnostic if the finding disappears. Overrides can specify
`kind` to distinguish declarations in separate Rust namespaces; ambiguous
unqualified overrides suppress nothing. A reasoned `[[exclude]]` entry can
instead suppress every finding under a module diagnostic path or in one
source file, for example generated source. Overrides and exclusions filter
output; they never add graph roots or preserve public visibility. This is
intentionally different from defining a production target: a production target
changes the analysis, while suppression records an accepted diagnostic scope.

## Fixes

Clippy can emit its suggestion at the point where a lint detects a problem,
and `cargo clippy --fix` arranges for Cargo to apply those compiler
suggestions. Hawk also delegates edits to `cargo fix`, but it cannot emit the
correct suggestion during its initial compiler invocation: the final answer
depends on fragments not yet collected from other targets and crates.

With `--fix`, Hawk therefore performs one or more fix phases:

```text
 collect fragments -> analyze -> build FixPlan
     |
     v
 cargo fix --lib <production owning packages>
 cargo fix --all-targets <non-production owning packages>
     |
     | HAWK_FIX_PLAN points the wrapper at selected declarations
     v
 rustc_driver emits MachineApplicable visibility suggestions
     |
     v
 re-run production, non-production, and compile-only doctest analysis
```

Only enabled, unsuppressed `hawk::unnecessary_public`,
`hawk::unnecessary_restricted_visibility`, and
`hawk::unnecessary_crate_visibility` findings enter a fix plan. Restricting
dead surface without removing it can activate rustc's ordinary `dead_code`
lint, so `hawk::dead_public` remains report-only. During fix compilations Hawk
caps ordinary compiler lints so Cargo consumes Hawk's planned suggestions
rather than unrelated compiler edits. It matches declarations across
recompilations by compiler ID or equivalent source identity and emits the
planned replacement.

The re-analysis matters: a `pub` to `pub(crate)` change can expose a further
module-local reduction, and any visibility change can alter downstream
compilation and the remaining graph. Hawk repeats the fix phase for newly
exposed restricted-visibility findings. The run succeeds only after the
selected production targets, non-production targets, and compile-only doctests
compile in their edited state.

## Implementation map

| File                           | Responsibility                                                                                                             |
| ------------------------------ | -------------------------------------------------------------------------------------------------------------------------- |
| `src/lib.rs`                   | Doc-hidden internal library target that shares the rustc-independent graph model, protocol, and analysis between binaries. |
| `src/main.rs`                  | User-facing command entry point.                                                                                           |
| `src/bin/cargo-hawk-driver.rs` | Internal compiler-wrapper entry point; the rustc-private implementation remains isolated to this binary.                   |
| `src/cli.rs`                   | Cargo metadata, production target selection, instrumented Cargo runs, lint levels, and the fix loop.                       |
| `src/diagnostics.rs`           | Rustc-shaped diagnostic rendering and source-file caching.                                                                 |
| `src/toolchain.rs`             | Compiler discovery, protocol validation, and dynamic-library setup.                                                        |
| `src/bin/driver/mod.rs`        | `rustc_driver` callback, HIR/type-based fragment collection, and suggestion emission during fix compilations.              |
| `src/graph.rs`                 | Serialized graph model, global reachability and visibility analysis, findings, and fix-plan representation.                |
| `src/config.rs`                | `hawk.toml` parsing, target selectors, production declarations, exact overrides, and scoped exclusions.                    |
| `tests/end_to_end.rs`          | User-facing behavior across Cargo builds, diagnostics, configuration, and fixes.                                           |

## References

Hawk:

- [README](../README.md)
- [Using Hawk](usage.md)
- [Configuration](configuration.md)
- [MVP design](mvp-design.md)
- [`src/lib.rs`](../src/lib.rs)
- [`src/cli.rs`](../src/cli.rs)
- [`src/bin/driver/mod.rs`](../src/bin/driver/mod.rs)
- [`src/graph.rs`](../src/graph.rs)

Clippy:

- [Clippy usage documentation](https://doc.rust-lang.org/clippy/usage.html)
- [Clippy Cargo frontend (`src/main.rs`)](https://github.com/rust-lang/rust-clippy/blob/master/src/main.rs)
- [Clippy compiler driver (`src/driver.rs`)](https://github.com/rust-lang/rust-clippy/blob/master/src/driver.rs)
- [Clippy lint pass registration (`clippy_lints/src/lib.rs`)](https://github.com/rust-lang/rust-clippy/blob/master/clippy_lints/src/lib.rs)
