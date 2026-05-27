# Architecture

Hawk applies a Clippy-shaped user experience to a question that cannot be
answered by a single crate compilation: which public declarations are
actually needed by a closed-world Cargo workspace product?

A binary product such as `uv` or `ruff` may be split across many internal
library crates. Rust requires cross-crate references to cross a `pub`
boundary, even when no external library API is intended. Rustc and Clippy see
each of those crate boundaries; Hawk additionally knows which workspace
binaries constitute the shipped product and which workspace targets consume
code only outside production.

This document describes the implementation on `main`. It compares Hawk with
Clippy's architecture and tooling model, not with the implementation of any
one Clippy lint.

## Comparison with Clippy

| Concern                      | Clippy                                                                                        | Hawk                                                                                             |
| ---------------------------- | --------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------ |
| User entry point             | `cargo clippy`, or `clippy-driver` directly                                                   | `cargo-hawk`                                                                                     |
| Cargo integration            | Sets `RUSTC_WORKSPACE_WRAPPER=clippy-driver`                                                  | Sets `RUSTC_WORKSPACE_WRAPPER` to the `cargo-hawk` executable itself                             |
| Compiler integration         | A `rustc_driver` callback registers Clippy lint passes with rustc's lint store                | A `rustc_driver` callback inspects analyzed HIR/type context and serializes graph fragments      |
| Unit of analysis             | One rustc invocation at a time                                                                | All instrumented crate compilations from the selected product and non-production surface         |
| When diagnostics are decided | During the compiler lint pass that finds the condition                                        | After Cargo completes, when graph fragments have been merged and traversed                       |
| Meaning of a public item     | Rust/Clippy lint semantics within the compilation being checked                               | A declaration that may be unnecessary under an explicit closed-world product model               |
| Suppression and severity     | Rust lint attributes and command-line lint levels, plus Clippy configuration where applicable | Clippy-style command-line levels plus exact, reasoned `hawk.toml` overrides                      |
| Automatic fixes              | The lint emits suggestions; `cargo clippy --fix` delegates application to `cargo fix`         | Graph analysis first writes a fix plan; a second compiler pass emits suggestions for `cargo fix` |

The shared foundation is deliberate. Both tools run as Cargo compiler
wrappers, use `rustc_private` compiler APIs, produce lint-style diagnostics,
and rely on Cargo's fix machinery for machine-applicable edits. Consequently,
both are coupled to a particular Rust compiler toolchain.

The architectural split is equally deliberate. A normal Clippy lint can
observe everything it needs during the crate currently being compiled. Hawk
must distinguish:

- an internal `pub` item reached from a selected production binary;
- an item needed across a crate boundary and therefore required to remain
  `pub`;
- an item used only by tests;
- an item never reached by any relevant consumer.

Those facts can originate in different Cargo target builds and different
crate compilations. Hawk therefore uses rustc as a fact collector and runs
its lint decision as a workspace-level post-processing step.

## Product Model

Hawk treats workspace library crates as internal implementation crates unless
the caller excludes them with `--exclude-crate`. It does not infer the product
from every binary that happens to compile. Each shipped binary is stated in
`hawk.toml`:

```toml
[[production]]
package = "uv"
bin = "uv"
reason = "shipped package manager binary"
```

Applicable `[[production]]` entries seed the production graph. Hawk also
compiles workspace non-production targets separately under
`cargo check --workspace --all-targets`:

- test harnesses seed a second reachability graph, allowing Hawk to report an
  item as needed only for tests;
- tests, benches, and examples preserve public visibility whenever a compiled
  cross-crate reference requires it;
- test-only compilation can expose `#[cfg(test)]` declarations and
  dev-dependency support crates as diagnostic candidates.

All instrumented builds currently use `--all-features` and one selected target
triple. A `target = "cfg(...)"` selector on a production entry or override
limits it to applicable target configurations.

This is the central semantic difference from Clippy. Running Clippy over a
workspace changes which crates are checked, but it does not define the
workspace as the complete external consumer of its internal libraries. Hawk's
configured product model does.

## Execution Pipeline

`cargo-hawk` is both the front-end executable and the compiler wrapper. Its
`main` function distinguishes an ordinary CLI invocation from a
`RUSTC_WORKSPACE_WRAPPER` invocation by checking Hawk's environment and the
wrapped `rustc` argument.

An analysis run proceeds as follows:

```text
 cargo-hawk
     |
     | read Cargo metadata, hawk.toml, lint levels, and target cfg
     v
 cargo check --package <product> --bin <product>   (once per product binary)
 cargo check --workspace --all-targets              (non-production surface)
     |
     | RUSTC_WORKSPACE_WRAPPER=cargo-hawk
     v
 Hawk rustc_driver callback after_analysis
     |
     | one JSON Fragment per compiled workspace crate
     v
 graph::analyze(production fragments, non-production fragments)
     |
     | apply hawk.toml overrides and command-line levels
     v
 rustc-shaped Hawk diagnostics
```

Every Cargo invocation includes `--all-features --locked`, a shared target
directory, and optional `--target`. Environment variables identify the
fragment output directory, selected product root, consumer mode, and run ID.
The run ID is tracked as compiler dependency input so Cargo does not reuse a
prior instrumented compilation without producing fresh fragments.

Clippy follows the first half of this shape: its Cargo frontend invokes Cargo
with `RUSTC_WORKSPACE_WRAPPER=clippy-driver`, and `clippy-driver` calls
`rustc_driver::run_compiler`. The difference is that Clippy's driver registers
lint passes in `config.register_lints`; those passes emit diagnostics as each
crate is checked. Hawk's callback runs after rustc analysis and emits data,
not findings, during the collection phase.

## Compiler Fragments

The wrapper records a `Fragment` for each compiled workspace crate. A fragment
contains:

- definitions, including source location, item kind, and whether the item is
  a public-surface candidate;
- typed reference edges extracted from bodies and public interfaces;
- entry-point roots when the crate is a selected product or test harness;
- conservative roots for code whose indirect execution cannot be safely
  recovered from direct call edges;
- required-public roots for visibility constraints that are independent of
  runtime reachability.

The public item coverage currently includes free functions, inherent methods
and associated constants, traits, named types, constants, statics, struct and
union fields, enum variants, named local re-exports, and public modules.
Macro-expanded declarations are not direct candidates.

For production compilation, candidates must be exported according to rustc's
effective visibility information. For a test-harness compilation, Hawk also
admits locally public declarations so it can analyze APIs compiled only for
tests without broadening the production surface.

### Edge Kinds

`src/driver.rs` produces five kinds of graph edge:

| Edge                    | Purpose                                                                                 |
| ----------------------- | --------------------------------------------------------------------------------------- |
| `Body`                  | A value, function, field, or variant is used by executable code.                        |
| `Interface`             | A declaration exposes another declaration in its type or ownership relationship.        |
| `Reexport`              | A named public `use` targets another declaration.                                       |
| `VisibilityParent`      | A public item is nested below a module whose visibility may also be required.           |
| `VisibilityRequirement` | A visibility relationship must be preserved even though it is not runtime reachability. |

Separating body reachability from visibility requirements is essential. An
item can be absent from product execution while still requiring `pub` because
some compiled cross-crate signature, re-export, or generated interface relies
on its visibility.

## Graph Analysis

`src/graph.rs` merges the fragments from all compiled graphs before emitting a
finding. The same source declaration may have separate compiler identities
when built for production and for non-production targets; Hawk merges those
identities using crate name, diagnostic path, item kind, and source span.

The analysis then computes two reachability closures:

- **production live** begins at each configured product binary entry point;
- **test live** begins at workspace test harness entry points.

Both closures include conservative roots, currently used for trait-associated
implementation code whose dispatch is not safely modeled by direct call
edges.

Separately, Hawk computes the declarations whose public visibility is
required. Any compiled cross-crate reference requires the referenced
declaration to retain visibility, regardless of whether the referencing item
is reachable from a selected root: rustc privacy-checks compiled code, not
only product-runtime code. The requirement propagates along interface,
re-export, visibility-parent, and explicit visibility-requirement edges.

For each public candidate in a non-excluded workspace library crate:

| State                                                           | Result                     |
| --------------------------------------------------------------- | -------------------------- |
| Not live in production or tests, and not required public        | `hawk::dead_public`        |
| Live in production or tests, but not required public            | `hawk::unnecessary_public` |
| Required public by a compiled cross-crate consumer or interface | no visibility finding      |

A selected production binary is a consumer, not a library surface to reduce,
so its crate does not receive these findings.

Enum variants are a special case. Hawk can report an unreachable public
variant as dead surface, but it does not report a reachable variant as
unnecessarily public because Rust does not provide an independent
`pub(crate)` modifier for a variant.

## Conservative Boundaries

Like Clippy, Hawk should avoid fixes that change valid code into code that no
longer compiles. Workspace-global analysis adds several privacy-specific
boundaries:

- A compiled cross-crate use preserves `pub` even if the use is outside
  product reachability.
- Named local re-exports are analyzed only where the target can be modeled
  soundly. Glob re-exports, module re-exports, and unmodeled or external
  targets remain conservative false negatives.
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

## Diagnostics and Configuration

Hawk uses Clippy-style ordered command-line levels:

```sh
cargo-hawk --manifest-path Cargo.toml -D warnings -W hawk::unnecessary_public
```

The visibility diagnostics are `hawk::dead_public` and
`hawk::unnecessary_public`. Configuration validation adds
`hawk::unknown_item` and `hawk::unfulfilled_expectation`.

Hawk's workspace-level decisions do not naturally map to source attributes in
a single crate compilation. Instead, `hawk.toml` carries exact, documented
exceptions:

```toml
[[override]]
lint = "hawk::unnecessary_public"
crate = "library"
item = "generated_registration"
level = "expect"
reason = "called by generated registration that Hawk does not model"
```

`allow` suppresses the selected finding. `expect` suppresses it while
producing a diagnostic if the finding disappears. Overrides filter output;
they never add graph roots or preserve public visibility. This is intentionally
different from adding a production consumer: a real consumer changes the
analysis, while an override records an accepted outstanding diagnostic.

## Fixes

Clippy can emit its suggestion at the point where a lint detects a problem,
and `cargo clippy --fix` arranges for Cargo to apply those compiler
suggestions. Hawk also delegates edits to `cargo fix`, but it cannot emit the
correct suggestion during its initial compiler invocation: the final answer
depends on fragments not yet collected from other targets and crates.

With `--fix`, Hawk therefore performs a second phase:

```text
 collect fragments -> analyze -> build FixPlan
     |
     v
 cargo fix --lib <production owning packages>
 cargo fix --all-targets <non-production owning packages>
     |
     | HAWK_FIX_PLAN points the wrapper at selected declarations
     v
 rustc_driver emits MachineApplicable "pub(crate)" suggestions
     |
     v
 re-run production and non-production collection and analysis
```

Only enabled, unsuppressed, actionable findings enter a fix plan. During fix
compilations Hawk caps ordinary compiler lints so Cargo consumes Hawk's
planned suggestions rather than unrelated compiler edits. It matches
declarations across recompilations by compiler ID or equivalent source
identity, emits `pub(crate)` replacements, and leaves enum-variant findings
report-only.

The final re-analysis matters: a visibility change can alter downstream
compilation and the remaining graph. The run succeeds only after the selected
product and non-production builds compile in their edited state.

## Implementation Map

| File                  | Responsibility                                                                                                   |
| --------------------- | ---------------------------------------------------------------------------------------------------------------- |
| `src/main.rs`         | Dispatch between the user-facing command and wrapper execution.                                                  |
| `src/cli.rs`          | Cargo metadata, product selection, instrumented Cargo runs, diagnostic rendering, lint levels, and the fix loop. |
| `src/driver.rs`       | `rustc_driver` callback, HIR/type-based fragment collection, and suggestion emission during fix compilations.    |
| `src/graph.rs`        | Serialized graph model, global reachability and visibility analysis, findings, and fix-plan representation.      |
| `src/config.rs`       | `hawk.toml` parsing, target selectors, production declarations, and exact override validation.                   |
| `tests/end_to_end.rs` | User-facing behavior across Cargo builds, diagnostics, configuration, and fixes.                                 |

## References

Hawk:

- [README](../README.md)
- [MVP design](mvp-design.md)
- [`src/cli.rs`](../src/cli.rs)
- [`src/driver.rs`](../src/driver.rs)
- [`src/graph.rs`](../src/graph.rs)

Clippy:

- [Clippy usage documentation](https://doc.rust-lang.org/clippy/usage.html)
- [Clippy Cargo frontend (`src/main.rs`)](https://github.com/rust-lang/rust-clippy/blob/master/src/main.rs)
- [Clippy compiler driver (`src/driver.rs`)](https://github.com/rust-lang/rust-clippy/blob/master/src/driver.rs)
- [Clippy lint pass registration (`clippy_lints/src/lib.rs`)](https://github.com/rust-lang/rust-clippy/blob/master/clippy_lints/src/lib.rs)
