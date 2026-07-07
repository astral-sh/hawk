# Configuration

Hawk reads `hawk.toml` from the workspace root by default. Use
`--config PATH` to select a different configuration file.

## Production targets

Declare every shipped binary in the analysis with a `[[production]]` entry:

```toml
[[production]]
package = "uv"
bin = "uv"
reason = "shipped package manager binary"

[[production]]
package = "uv-dev"
bin = "uv-dev"
reason = "developer binary shipped from this workspace"

[[production]]
package = "windows-helper"
bin = "windows-helper"
target = "cfg(windows)"
reason = "Windows-only binary shipped from this workspace"
```

Each applicable production target is built for analysis, so declarations
required by that binary are not reported as dead or unnecessarily public.
Every package and binary must be a target of the selected Cargo workspace. At
least one production target must apply to the analyzed target.

All configured binaries are analyzed with the same feature profiles and
compilation target. Hawk intentionally does not infer production targets from
the workspace: configure each intended target explicitly.

## Feature profiles

By default, Hawk performs one analysis with Cargo's `--all-features` option.
Configure a feature matrix when code that is required with a feature disabled
would otherwise be absent from that build:

```toml
[[feature-profile]]
name = "all"
all-features = true

[[feature-profile]]
name = "minimal"
no-default-features = true

[[feature-profile]]
name = "serde-only"
no-default-features = true
features = ["serde"]
```

Hawk compiles every production binary, workspace non-production target, and
doctest under each profile. Fragments are stored separately for each profile,
then their reachability and visibility requirements are combined before
diagnostics are produced. A declaration required in any configured profile is
therefore preserved.

Profile names must be unique and contain only ASCII letters, digits, `-`, or
`_`. `all-features = true` cannot be combined with `no-default-features` or an
explicit `features` list. A profile with none of those settings uses Cargo's
default features. Each string in `features` is passed as a separate Cargo
`--features` value.

Automatic fixes are currently rejected when multiple feature profiles are
configured. Applying a visibility change safely across several configurations
requires a coordinated fix plan; run the matrix without `--fix`, or select a
single profile for a fixing run. Feature profiles do not select compilation
targets; `--target` still selects one target for the entire analysis.

## Uniform field visibility

Set `preserve-uniform-field-visibility = true` to retain a struct or union's
intentional uniform field visibility:

```toml
preserve-uniform-field-visibility = true
```

When every source-written field has the same visibility and at least one field
semantically requires that visibility, Hawk does not suggest reducing the
visibility of its siblings. The policy applies to public and restricted
visibility reductions, but does not suppress `hawk::dead_public`.

This setting is disabled by default. It does not apply to mixed-visibility
declarations or declarations whose complete source field list is unavailable,
such as macro-generated structs.

## Overrides

An override records an intentional finding without changing the analysis:

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
kind = "function"
level = "expect"
reason = "called by generated registration that Hawk does not model"

[[override]]
lint = "hawk::unnecessary_restricted_visibility"
crate = "library"
item = "platform::shared_helper"
level = "expect"
reason = "called by generated platform code that Hawk does not model"

[[override]]
lint = "hawk::dead_public"
crate = "platform"
item = "windows_only_api"
level = "expect"
target = "cfg(windows)"
reason = "public API retained only in the Windows build"
```

`allow` suppresses a matching finding. `expect` suppresses a matching finding
and reports `hawk::unfulfilled_expectation` if that finding is no longer
present. An override whose selectors no longer identify a compiled item
reports `hawk::unknown_item`. If an override without `kind` identifies
multiple same-named declarations, Hawk reports `hawk::ambiguous_item` and
suppresses none of them.

Definitions from the same Cargo package with the same crate, diagnostic path,
and item kind are one logical override identity even when cfg alternatives
compile them from different source locations. An override applies to every
such physical variant, and an `expect` is fulfilled when at least one variant
produces the selected finding. Workspace library crate names are required to
be unique, so definitions in different packages cannot share an override
identity. Same-path declarations in different Rust namespaces remain
ambiguous unless `kind` is supplied.

The `item` value names Hawk's diagnostic path. For exported aliases, use the
alias name, such as `PublicAlias`; for modules, use the module path, such as
`api::internal`. Add `kind` when separate Rust namespaces define declarations
with the same path. It accepts Hawk's item-kind names, such as `function`,
`type_alias`, and `constant`.

Overrides filter diagnostics only. Unlike a `[[production]]` entry, an
override does not define a production target, establish reachability, or
preserve public visibility for referenced declarations.

## Exclusions

Use an exclusion when an entire module subtree or source file is outside the
diagnostic surface, for example generated code:

```toml
[[exclude]]
crate = "library"
module = "generated_bindings"
reason = "generated from the protocol schema"

[[exclude]]
crate = "platform"
file = "platform/src/generated.rs"
reason = "generated platform bindings"
```

An exclusion must provide exactly one of `module` or `file`. A module selector
uses Hawk's diagnostic path and suppresses the selected module and all
descendants, such as `generated_bindings::Message`. A file selector matches
the source path printed in diagnostics. Both forms suppress all Hawk findings
in their selected scope.

Exclusions filter diagnostics and fixes only; they do not change reachability
or preserve visibility. Prefer an exact `[[override]]` when an individual
diagnostic is an intentional exception that should remain audited with
`expect`. Exclusions are not expectations and do not diagnose a selected
scope that currently produces no findings.

## Target selectors

`[[production]]`, `[[override]]`, and `[[exclude]]` accept an optional
`target`. The value uses the same named targets and `cfg(...)` platform
expressions as Cargo target dependencies:

```toml
[[production]]
package = "windows-helper"
bin = "windows-helper"
target = "cfg(windows)"
reason = "Windows-only production binary"

[[override]]
lint = "hawk::dead_public"
crate = "platform"
item = "windows_fallback"
level = "expect"
target = "cfg(not(windows))"
reason = "non-Windows compatibility surface"
```

An entry is validated only when its selector applies to the analyzed
compilation target. This avoids stale-expectation failures for declarations
that are not compiled on that target.

## External library boundaries

`hawk.toml` defines production targets and diagnostic exceptions. To omit an
entire workspace library crate because it exposes a supported API outside the
closed-world analysis, pass `--exclude-crate`:

```sh
./target/debug/cargo-hawk \
  --manifest-path /path/to/workspace/Cargo.toml \
  --exclude-crate supported_library
```

Excluded crates are compiled as required by Cargo, but Hawk does not report
their public declarations.
