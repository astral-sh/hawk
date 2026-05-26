# MVP design

## Product model

The analyzed unit is one explicitly selected Cargo binary target. All
workspace library dependencies compiled into that binary are considered
closed-world unless their crates are explicitly excluded.

The MVP analyzes:

- one binary target;
- `--all-features`;
- one selected compilation target, defaulting to the host target;
- production reachability only.

Alternate products and test-only reachability are future analysis modes rather
than implicit roots.

## Diagnostics

The selected binary seeds reachability but does not itself receive `hawk`
diagnostics. Compiled workspace library dependencies can produce:

- dead public surface: a public declaration is not reachable from the product;
- unnecessary public visibility: a live public declaration has no live
  cross-crate consumer and can be restricted to `pub(crate)`.

Hawk emits warnings and exits successfully by default. Clippy-style ordered
`-A`/`-W`/`-D` options control lint levels; `-D warnings` enforces all Hawk
diagnostics in CI, while a later per-diagnostic option can incrementally lower
or allow one lint. The options apply after `hawk.toml` overrides and cover both
visibility findings and configuration diagnostics for stale selectors or
unfulfilled expectations. Invalid configuration and instrumented build
failures fail independently of lint levels. Hawk retains declaration spans and
reasoning needed for follow-up machine-applicable fixes.

## Initial scope

The MVP includes free functions, inherent methods and associated functions,
traits, named types, constants, and statics. Public re-exports are recorded in graph
fragments but do not produce diagnostics yet: rustc resolves downstream paths
to the underlying declaration, so an export-path diagnostic cannot yet prove
which `use` was consumed. Targets of public re-exports are treated as
required-public roots because narrowing only the declaration fails with
`E0365`. Proc-macro entry points are also treated as required-public roots
because rustc requires those attributed functions to remain public. The MVP
suggests no visibility narrower than `pub(crate)`.

Fields, enum variants, and public module visibility are deferred. Direct
trait-associated item diagnostics are represented by the containing trait,
because trait items do not carry their own visibility. Types assigned by
associated type definitions in publicly reachable trait implementations are
treated as required-public roots because restricting them can make the crate
fail to compile (`E0446`) even without a product call path. Trait method
interface edges are recorded so a type returned across a compiled crate
boundary also remains public. Trait implementation bodies are conservatively
rooted so indirect trait dispatch does not turn into dead-public false
positives. Any compiled cross-crate reference prevents a visibility diagnostic
because rustc privacy-checks dead items as well as production-reachable ones
and `pub` is the narrowest Rust visibility available for those uses.

An optional workspace-root `hawk.toml` configures diagnostic overrides by
exact lint, crate, and item path, optionally scoped to a Cargo-style target
name or `cfg(...)` platform expression. `allow` suppresses a matching finding;
`expect` also produces an unfulfilled-expectation diagnostic when its finding
disappears on an applicable target. Overrides that refer to an item absent
from an applicable compiled graph produce an unknown-item diagnostic so stale
configuration is visible. Overrides do not change reachability or
required-public analysis.

## Implementation direction

`cargo hawk` invokes the selected Cargo build with
`RUSTC_WORKSPACE_WRAPPER=hawk-driver`. The compiler driver is pinned to the
workspace Rust toolchain and emits resolved graph fragments for each compiled
workspace crate. The frontend merges those fragments and traverses from the
selected binary entry point. If Cargo compiles the same workspace crate more
than once, equivalent source declarations are merged by crate, name, kind, and
span before diagnostics are emitted.

Reference edges distinguish implementation-body reachability from public
interface exposure. Cross-crate references from all compiled items preserve
the referenced declaration's public visibility; interface edges then preserve
types exposed through that declaration, including trait method return types.
