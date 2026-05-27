# Configuration

Hawk reads `hawk.toml` from the workspace root by default. Use
`--config PATH` to select a different configuration file.

## Production consumers

List every binary shipped as part of the analyzed product with a
`[[production]]` entry:

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

Each applicable entry is built as a production consumer, so declarations
required by that binary are not reported as dead or unnecessarily public.
Every package and binary must be a target of the selected Cargo workspace. At
least one production binary must apply to the analyzed target.

All configured binaries are analyzed with the same `--all-features` and
compilation target. Hawk intentionally does not infer product binaries from
the workspace: if a binary is an intended consumer, configure it explicitly.

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
and reports `hawk::unfulfilled_expectation` if that finding is no longer
present. An override whose `crate` and `item` selector no longer identifies a
compiled item reports `hawk::unknown_item`.

The `item` value names Hawk's diagnostic path. For exported aliases, use the
alias name, such as `PublicAlias`; for modules, use the module path, such as
`api::internal`.

Overrides filter diagnostics only. Unlike a `[[production]]` entry, an
override does not add a consumer, establish reachability, or preserve public
visibility for referenced declarations.

## Target selectors

Both `[[production]]` and `[[override]]` accept an optional `target`. The
value uses the same named targets and `cfg(...)` platform expressions as Cargo
target dependencies:

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

`hawk.toml` defines product consumers and diagnostic exceptions. To omit an
entire workspace library crate because it exposes a supported API outside the
closed-world product, pass `--exclude-crate`:

```sh
./target/debug/cargo-hawk \
  --manifest-path /path/to/workspace/Cargo.toml \
  --exclude-crate supported_library
```

Excluded crates are compiled as required by Cargo, but Hawk does not report
their public declarations.
