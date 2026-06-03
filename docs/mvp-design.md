# MVP design

## Product model

The analyzed surface is the workspace library dependencies compiled into
configured same-workspace production binary targets or workspace
non-production targets. They are considered closed-world unless their crates
are explicitly excluded. The configured production targets establish
production reachability. Workspace tests are compiled as part of a separate
non-production graph and can also introduce test-only declarations;
benches, examples, and doctests in that graph preserve any public visibility
they require.

The analysis includes:

- one or more configured same-workspace binary targets;
- `--all-features`;
- one selected compilation target, defaulting to the host target;
- production reachability from those binary targets;
- test reachability and visibility requirements from workspace
  non-production targets, including doctests.

Binaries are never inferred as roots: every shipped binary must be listed
explicitly in `hawk.toml`.

## Diagnostics

Configured production targets seed production reachability but do not
themselves receive `hawk` diagnostics. Non-production targets can preserve
public visibility; tests additionally establish test-only reachability and
introduce diagnostic candidates from dev-dependencies or `#[cfg(test)]`
source. Compiled workspace library declarations in either graph can
produce:

- dead public surface: a public declaration is not reachable from production
  or workspace tests;
- unnecessary public visibility: a production-live or test-live public
  declaration has no compiled cross-crate consumer and can be restricted to
  `pub(crate)`;
- unnecessary restricted visibility: an explicit restricted visibility
  modifier has compiled uses only within its defining module and can be
  removed;
- unnecessary crate visibility: an explicit `pub(crate)` declaration has
  compiled uses only within its parent module and can optionally be
  `pub(super)`.

Hawk emits warnings and exits successfully by default. Clippy-style ordered
`-A`/`-W`/`-D` options control lint levels; `-D warnings` enforces the
warn-by-default Hawk diagnostics in CI, while a later per-diagnostic option can
incrementally lower or allow one lint. `hawk::unnecessary_crate_visibility`
remains allow-by-default until explicitly enabled. The options apply after
`hawk.toml` suppressions and cover both visibility findings and configuration
diagnostics for stale selectors or unfulfilled expectations. Invalid
configuration and instrumented build failures fail independently of lint
levels. With `--fix`, Hawk converts
enabled, unsuppressed visibility-reduction findings to machine-applicable
suggestions and delegates editing and validation to `cargo fix`. Dead-public
findings remain report-only because narrowing unused surface can activate
rustc's `dead_code` lint.

## Initial scope

The MVP includes free functions, inherent methods and associated constants,
traits, named types, constants, statics, public struct and union fields, enum
variants, selected public re-exports, and public modules. For a named public
re-export of a modeled local non-module declaration, Hawk diagnoses the
exported path only if no compiled cross-crate reference to its target (or a
required interface related to it) exists. A target unreachable from the
configured production target produces a dead-export finding; a target used without a
possible external consumer produces an unnecessary-public finding for the
`use`. Targets of public re-exports remain required-public roots because
narrowing only the declaration fails with `E0365`.

Field construction, projection, named-pattern, and `offset_of!` uses are
reference edges, including tuple-struct constructors whose accessibility
depends on their fields. A field interface edge preserves any type exposed
through a public field that remains required across a crate boundary. Enum
variant constructors and paths are edges to the specific variant, and fields
and variants preserve their containing type. Unlike fields and inherent
associated constants, enum variants cannot be changed to `pub(crate)`. Hawk
reports unreachable variants as dead public surface that may be removed along
with any remaining unreachable uses, but does not emit an unnecessary-public
finding for a reachable variant because it has no independent actionable
visibility change.

Rustc resolves downstream uses of an exported path to its underlying
declaration; the current graph cannot recover which `pub use` was consumed.
The sound conservative boundary is deliberate: glob re-exports and
re-exports of modules or unmodeled/external targets do not produce findings,
and a public module containing a public re-export is retained. This avoids
recommending visibility changes that could make a consumer fail privacy
checking.

Public module definitions are linked to lexically contained definitions by
visibility-parent edges. A compiled cross-crate reference to a descendant
retains every public declaring module on that path, even where a different
re-export might also expose the descendant. This can miss unnecessary module
visibility, but does not suggest narrowing a path required by known
consumers. Proc-macro entry points are also treated as required-public roots
because rustc requires those attributed functions to remain public.

Explicit restricted visibility modifiers are analyzed against the lexical
module scope of every compiled reference. Hawk suggests private visibility
when all uses fit within the defining module. For exact `pub(crate)`
declarations, Hawk can optionally suggest `pub(super)` when all uses fit
within the parent module. Crate-visible re-exports remain conservative false
negatives because rustc does not preserve enough exported-path provenance to
prove a narrower replacement.

Direct trait-associated item diagnostics are represented by the containing
trait, because trait items do not carry their own visibility. Types assigned
by associated type definitions in publicly reachable trait implementations are
treated as required-public roots because restricting them can make the crate
fail to compile (`E0446`) even without a production call path. Trait method
interface edges are recorded so a type returned across a compiled crate
boundary also remains public. Trait implementation bodies are conservatively
rooted so indirect trait dispatch does not turn into dead-public false
positives. Any compiled cross-crate reference prevents a visibility diagnostic
because rustc privacy-checks dead items as well as production-reachable ones
and `pub` is the narrowest Rust visibility available for those uses.

An optional workspace-root `hawk.toml` configures production targets,
diagnostic policy, overrides, and broad diagnostic exclusions. The opt-in
`preserve-uniform-field-visibility` policy omits reducible field-visibility
findings when a complete, uniformly visible field group has a sibling that
semantically requires the current visibility. A `[[production]]` entry names a
package and binary target in the selected workspace, optionally scoped to a
Cargo-style target name or `cfg(...)` platform expression, and its compiled
references participate in production reachability and required-public
analysis. An `[[override]]` entry identifies an exact lint, crate, and item
path under the same optional target scoping, with an optional item kind to
disambiguate declarations in separate Rust namespaces. `allow` suppresses a
matching finding; `expect` also produces an unfulfilled-expectation diagnostic
when its finding disappears on an applicable target. Overrides that refer to
an item absent from an applicable compiled graph produce an unknown-item
diagnostic; selectors matching multiple declarations produce an
ambiguous-item diagnostic and suppress nothing. Overrides do not change
reachability or required-public analysis, and suppressed findings are not
eligible for fixes. A `[[exclude]]` entry names a crate and either a module
subtree or source file, suppressing all findings in that scope without changing
the analysis; it is intended for source areas such as generated code.

## Implementation direction

`cargo hawk` invokes each configured production target build, `cargo check
--workspace --all-targets`, and compile-only workspace doctests with
`RUSTC_WORKSPACE_WRAPPER=hawk-driver`. The doctest pass uses rustdoc's
test-builder wrapper so documentation example references are emitted into the
same non-production graph. The compiler
driver is pinned to the workspace Rust toolchain and emits resolved graph
fragments for each compiled workspace crate. The frontend retains production
and test root sets, so a declaration can be production-live, test-live, or
dead, while combining compiled cross-crate visibility requirements from all
passes. If Cargo compiles the same workspace crate more than once, equivalent
source declarations are merged by crate, name, kind, and span before
diagnostics are emitted. In a library test harness, Hawk additionally records
source-level public declarations, and admits those absent from the production
pass as test surface candidates, so items enabled only under `#[cfg(test)]`
are analyzed without broadening the existing production candidate surface.

Reference edges distinguish implementation-body reachability from public
interface exposure. Cross-crate references from all compiled items preserve
the referenced declaration's public visibility; interface edges then preserve
types exposed through that declaration, including trait method return types.
Visibility-parent edges preserve public lexical module paths for declarations
that a compiled external item may access. Public re-export candidates are
checked against this required-visibility closure and are reported only when
the target kind and absence of potential external consumers make narrowing
provably type-checking-safe. Lexical module scopes attached to definitions
also let the merged graph prove narrower visibility for explicit restricted
visibility modifiers.

Fixing uses additional compilation phases because findings are determined
only after Hawk merges graph fragments. Hawk builds fix plans from emitted
visibility-reduction findings, running package-scoped `cargo fix --lib` for
production findings and package-scoped `cargo fix --all-targets` for findings
reached through or declared only in the non-production graph. The latter
compiles each owning library while retaining test configuration and validating
benches and examples, so declarations in dev-dependency support libraries and
declarations enabled under `#[cfg(test)]` can be edited. Fix compilations cap ordinary
compiler lints to prevent Cargo from consuming unrelated rustc suggestions;
Hawk's compiler wrapper matches equivalent declaration identities and emits
the planned rustc `MachineApplicable` suggestions. Hawk rechecks every
configured production target and workspace non-production target, including
compile-only doctests, after each round and applies newly exposed visibility
reductions before completion.
