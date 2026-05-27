use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use cargo_platform::{Cfg, Platform};
use serde::Deserialize;

use crate::graph::{Definition, DefinitionKind, Finding, FindingKind, Fragment};

#[derive(Debug, Default)]
pub struct Config {
    path: Option<PathBuf>,
    source: String,
    overrides: Vec<LintOverride>,
    production: Vec<ProductionConsumer>,
}

#[derive(Clone, Debug)]
pub struct LintOverride {
    pub lint: FindingKind,
    pub crate_name: String,
    pub item: String,
    pub definition_kind: Option<DefinitionKind>,
    pub level: OverrideLevel,
    pub reason: String,
    pub target: Option<Platform>,
    pub span: ConfigSpan,
}

#[derive(Clone, Debug)]
pub struct ProductionConsumer {
    pub package: String,
    pub binary: String,
    pub reason: String,
    pub target: Option<Platform>,
    pub span: ConfigSpan,
}

#[derive(Debug)]
pub struct AnalysisTarget {
    name: String,
    cfgs: Vec<Cfg>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum OverrideLevel {
    Allow,
    Expect,
}

#[derive(Clone, Copy, Debug)]
pub struct ConfigSpan {
    pub line: usize,
    pub column: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfigDiagnosticKind {
    UnknownItem,
    AmbiguousItem,
    UnfulfilledExpectation,
}

#[derive(Clone, Copy, Debug)]
pub struct ConfigDiagnostic<'a> {
    pub kind: ConfigDiagnosticKind,
    pub entry: &'a LintOverride,
}

pub struct AppliedFindings<'findings, 'config> {
    pub findings: Vec<Finding<'findings>>,
    pub config_diagnostics: Vec<ConfigDiagnostic<'config>>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    #[serde(default, rename = "override")]
    overrides: Vec<toml::Spanned<RawLintOverride>>,
    #[serde(default)]
    production: Vec<toml::Spanned<RawProductionConsumer>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLintOverride {
    lint: String,
    #[serde(rename = "crate")]
    crate_name: String,
    item: String,
    #[serde(rename = "kind")]
    definition_kind: Option<DefinitionKind>,
    level: OverrideLevel,
    reason: String,
    target: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawProductionConsumer {
    package: String,
    #[serde(rename = "bin")]
    binary: String,
    reason: String,
    target: Option<String>,
}

impl Config {
    pub fn load(workspace_root: &Path, configured_path: Option<&Path>) -> Result<Self> {
        let path = configured_path
            .map(Path::to_path_buf)
            .unwrap_or_else(|| workspace_root.join("hawk.toml"));
        let source = match fs::read_to_string(&path) {
            Ok(source) => source,
            Err(error)
                if error.kind() == std::io::ErrorKind::NotFound && configured_path.is_none() =>
            {
                return Ok(Self::default());
            }
            Err(error) => {
                return Err(error).with_context(|| format!("read {}", path.display()));
            }
        };
        let raw: RawConfig =
            toml::from_str(&source).with_context(|| format!("parse {}", path.display()))?;
        let mut overrides = Vec::new();
        for entry in raw.overrides {
            let span = config_span(&source, entry.span().start);
            let entry = entry.into_inner();
            let lint = FindingKind::from_code(&entry.lint).ok_or_else(|| {
                anyhow::anyhow!(
                    "unknown Hawk lint `{}` in {}:{}:{}",
                    entry.lint,
                    path.display(),
                    span.line,
                    span.column
                )
            })?;
            if entry.reason.trim().is_empty() {
                bail!(
                    "override in {}:{}:{} must provide a non-empty reason",
                    path.display(),
                    span.line,
                    span.column
                );
            }
            let target = entry
                .target
                .map(|target| {
                    target.parse::<Platform>().with_context(|| {
                        format!(
                            "parse target selector `{target}` in {}:{}:{}",
                            path.display(),
                            span.line,
                            span.column
                        )
                    })
                })
                .transpose()?;
            overrides.push(LintOverride {
                lint,
                crate_name: entry.crate_name,
                item: entry.item,
                definition_kind: entry.definition_kind,
                level: entry.level,
                reason: entry.reason,
                target,
                span,
            });
        }
        let mut production = Vec::new();
        for entry in raw.production {
            let span = config_span(&source, entry.span().start);
            let entry = entry.into_inner();
            if entry.reason.trim().is_empty() {
                bail!(
                    "production consumer in {}:{}:{} must provide a non-empty reason",
                    path.display(),
                    span.line,
                    span.column
                );
            }
            let target = entry
                .target
                .map(|target| {
                    target.parse::<Platform>().with_context(|| {
                        format!(
                            "parse target selector `{target}` in {}:{}:{}",
                            path.display(),
                            span.line,
                            span.column
                        )
                    })
                })
                .transpose()?;
            production.push(ProductionConsumer {
                package: entry.package,
                binary: entry.binary,
                reason: entry.reason,
                target,
                span,
            });
        }
        Ok(Self {
            path: Some(path),
            source,
            overrides,
            production,
        })
    }

    pub fn production_consumers(
        &self,
        target: &AnalysisTarget,
    ) -> impl Iterator<Item = &ProductionConsumer> {
        self.production
            .iter()
            .filter(move |consumer| consumer.applies_to(target))
    }

    pub fn apply<'findings, 'config>(
        &'config self,
        target: &AnalysisTarget,
        production_fragments: &[Fragment],
        test_fragments: &[Fragment],
        findings: Vec<Finding<'findings>>,
    ) -> AppliedFindings<'findings, 'config> {
        let known_items: HashSet<KnownItemIdentity<'_>> = production_fragments
            .iter()
            .chain(test_fragments)
            .flat_map(|fragment| &fragment.definitions)
            .map(known_item_identity)
            .collect();
        let mut config_diagnostics = Vec::new();
        let mut active_overrides = Vec::new();
        for entry in self
            .overrides
            .iter()
            .filter(|entry| entry.applies_to(target))
        {
            let matching_items = known_items
                .iter()
                .filter(|item| entry.identifies(item))
                .count();
            if matching_items == 0 {
                config_diagnostics.push(ConfigDiagnostic {
                    kind: ConfigDiagnosticKind::UnknownItem,
                    entry,
                });
                continue;
            }
            if matching_items > 1 {
                config_diagnostics.push(ConfigDiagnostic {
                    kind: ConfigDiagnosticKind::AmbiguousItem,
                    entry,
                });
                continue;
            }
            active_overrides.push(entry);
            if entry.level == OverrideLevel::Expect
                && !findings.iter().any(|finding| entry.matches(finding))
            {
                config_diagnostics.push(ConfigDiagnostic {
                    kind: ConfigDiagnosticKind::UnfulfilledExpectation,
                    entry,
                });
            }
        }
        let findings = findings
            .into_iter()
            .filter(|finding| !active_overrides.iter().any(|entry| entry.matches(finding)))
            .collect();
        AppliedFindings {
            findings,
            config_diagnostics,
        }
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn source_line(&self, line: usize) -> Option<&str> {
        self.source.lines().nth(line.checked_sub(1)?)
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct KnownItemIdentity<'a> {
    crate_name: &'a str,
    item: &'a str,
    kind: DefinitionKind,
    file: Option<&'a str>,
    line: Option<usize>,
    column: Option<usize>,
}

fn known_item_identity(definition: &Definition) -> KnownItemIdentity<'_> {
    KnownItemIdentity {
        crate_name: definition.crate_name.as_str(),
        item: definition.name.as_str(),
        kind: definition.kind,
        file: definition.span.as_ref().map(|span| span.file.as_str()),
        line: definition.span.as_ref().map(|span| span.line),
        column: definition.span.as_ref().map(|span| span.column),
    }
}

impl AnalysisTarget {
    pub fn from_rustc(target: Option<&str>) -> Result<Self> {
        let name = match target {
            Some(target) => target.to_owned(),
            None => {
                let output = Command::new("rustc")
                    .arg("-vV")
                    .output()
                    .context("query rustc host target")?;
                if !output.status.success() {
                    bail!("query rustc host target failed with {}", output.status);
                }
                let stdout = String::from_utf8(output.stdout).context("decode rustc version")?;
                stdout
                    .lines()
                    .find_map(|line| line.strip_prefix("host: "))
                    .context("rustc version did not report a host target")?
                    .to_owned()
            }
        };
        let mut rustc = Command::new("rustc");
        rustc.arg("--print=cfg");
        if let Some(target) = target {
            rustc.arg("--target").arg(target);
        }
        let output = rustc
            .output()
            .with_context(|| format!("query rustc configuration for target `{name}`"))?;
        if !output.status.success() {
            bail!(
                "query rustc configuration for target `{name}` failed with {}",
                output.status
            );
        }
        let stdout = String::from_utf8(output.stdout)
            .with_context(|| format!("decode rustc configuration for target `{name}`"))?;
        let cfgs = stdout
            .lines()
            .map(|line| {
                line.parse::<Cfg>()
                    .with_context(|| format!("parse rustc configuration `{line}`"))
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Self { name, cfgs })
    }
}

impl LintOverride {
    fn applies_to(&self, target: &AnalysisTarget) -> bool {
        self.target
            .as_ref()
            .is_none_or(|platform| platform.matches(&target.name, &target.cfgs))
    }

    fn identifies(&self, item: &KnownItemIdentity<'_>) -> bool {
        self.crate_name == item.crate_name
            && self.item == item.item
            && self
                .definition_kind
                .is_none_or(|definition_kind| definition_kind == item.kind)
    }

    fn matches(&self, finding: &Finding<'_>) -> bool {
        self.lint == finding.kind
            && self.crate_name == finding.definition.crate_name
            && self.item == finding.definition.name
            && self
                .definition_kind
                .is_none_or(|kind| kind == finding.definition.kind)
    }
}

impl ProductionConsumer {
    fn applies_to(&self, target: &AnalysisTarget) -> bool {
        self.target
            .as_ref()
            .is_none_or(|platform| platform.matches(&target.name, &target.cfgs))
    }
}

fn config_span(source: &str, offset: usize) -> ConfigSpan {
    let prefix = &source[..offset];
    let line = prefix.bytes().filter(|byte| *byte == b'\n').count() + 1;
    let column = prefix
        .rsplit_once('\n')
        .map_or(prefix.chars().count() + 1, |(_, line)| {
            line.chars().count() + 1
        });
    ConfigSpan { line, column }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use cargo_platform::Cfg;

    use super::{AnalysisTarget, Config, ConfigDiagnosticKind};
    use crate::graph::{Definition, DefinitionKind, FindingKind, Fragment, analyze};

    fn fragment() -> Fragment {
        Fragment {
            crate_name: "library".into(),
            crate_id: "library".into(),
            is_product_root: false,
            definitions: vec![Definition {
                id: "unused".into(),
                crate_name: "library".into(),
                name: "unused".into(),
                kind: DefinitionKind::Function,
                span: None,
                public_api: true,
            }],
            edges: vec![],
            roots: vec![],
            conservative_roots: vec![],
            required_public_roots: vec![],
        }
    }

    fn target(name: &str, cfgs: &[&str]) -> AnalysisTarget {
        AnalysisTarget {
            name: name.into(),
            cfgs: cfgs
                .iter()
                .map(|cfg| cfg.parse::<Cfg>().expect("valid target cfg"))
                .collect(),
        }
    }

    fn candidate_crates() -> HashSet<String> {
        HashSet::from(["library".to_owned()])
    }

    fn same_named_fragment() -> Fragment {
        let mut fragment = fragment();
        fragment.definitions = vec![
            Definition {
                id: "alias".into(),
                crate_name: "library".into(),
                name: "SameName".into(),
                kind: DefinitionKind::TypeAlias,
                span: None,
                public_api: true,
            },
            Definition {
                id: "constant".into(),
                crate_name: "library".into(),
                name: "SameName".into(),
                kind: DefinitionKind::Constant,
                span: None,
                public_api: true,
            },
        ];
        fragment
    }

    #[test]
    fn expect_suppresses_a_matching_finding() {
        let directory = tempfile::tempdir().expect("temporary configuration directory");
        let path = directory.path().join("hawk.toml");
        std::fs::write(
            &path,
            r#"
[[override]]
lint = "hawk::dead_public"
crate = "library"
item = "unused"
level = "expect"
reason = "known retained public surface"
"#,
        )
        .expect("write configuration");
        let config = Config::load(directory.path(), Some(&path)).expect("load configuration");
        let fragments = vec![fragment()];
        let findings = analyze(&fragments, &[], &candidate_crates(), &HashSet::new());

        let applied = config.apply(
            &target("aarch64-apple-darwin", &["unix"]),
            &fragments,
            &[],
            findings,
        );

        assert!(applied.findings.is_empty());
        assert!(applied.config_diagnostics.is_empty());
    }

    #[test]
    fn missing_item_is_reported_instead_of_unfulfilled_expectation() {
        let directory = tempfile::tempdir().expect("temporary configuration directory");
        let path = directory.path().join("hawk.toml");
        std::fs::write(
            &path,
            r#"
[[override]]
lint = "hawk::dead_public"
crate = "library"
item = "removed"
level = "expect"
reason = "detect stale selectors"
"#,
        )
        .expect("write configuration");
        let config = Config::load(directory.path(), Some(&path)).expect("load configuration");
        let fragments = vec![fragment()];
        let findings = analyze(&fragments, &[], &candidate_crates(), &HashSet::new());

        let applied = config.apply(
            &target("aarch64-apple-darwin", &["unix"]),
            &fragments,
            &[],
            findings,
        );

        assert_eq!(applied.findings.len(), 1);
        assert_eq!(applied.findings[0].kind, FindingKind::DeadPublic);
        assert_eq!(applied.config_diagnostics.len(), 1);
        assert_eq!(
            applied.config_diagnostics[0].kind,
            ConfigDiagnosticKind::UnknownItem
        );
    }

    #[test]
    fn ambiguous_item_selector_suppresses_no_findings() {
        let directory = tempfile::tempdir().expect("temporary configuration directory");
        let path = directory.path().join("hawk.toml");
        std::fs::write(
            &path,
            r#"
[[override]]
lint = "hawk::dead_public"
crate = "library"
item = "SameName"
level = "expect"
reason = "ambiguous Rust namespace"
"#,
        )
        .expect("write configuration");
        let config = Config::load(directory.path(), Some(&path)).expect("load configuration");
        let fragments = vec![same_named_fragment()];
        let findings = analyze(&fragments, &[], &candidate_crates(), &HashSet::new());

        let applied = config.apply(
            &target("aarch64-apple-darwin", &["unix"]),
            &fragments,
            &[],
            findings,
        );

        assert_eq!(applied.findings.len(), 2);
        assert_eq!(applied.config_diagnostics.len(), 1);
        assert_eq!(
            applied.config_diagnostics[0].kind,
            ConfigDiagnosticKind::AmbiguousItem
        );
    }

    #[test]
    fn definition_kind_disambiguates_an_override() {
        let directory = tempfile::tempdir().expect("temporary configuration directory");
        let path = directory.path().join("hawk.toml");
        std::fs::write(
            &path,
            r#"
[[override]]
lint = "hawk::dead_public"
crate = "library"
item = "SameName"
kind = "type_alias"
level = "expect"
reason = "retain the type alias"
"#,
        )
        .expect("write configuration");
        let config = Config::load(directory.path(), Some(&path)).expect("load configuration");
        let fragments = vec![same_named_fragment()];
        let findings = analyze(&fragments, &[], &candidate_crates(), &HashSet::new());

        let applied = config.apply(
            &target("aarch64-apple-darwin", &["unix"]),
            &fragments,
            &[],
            findings,
        );

        assert_eq!(applied.findings.len(), 1);
        assert_eq!(
            applied.findings[0].definition.kind,
            DefinitionKind::Constant
        );
        assert!(applied.config_diagnostics.is_empty());
    }

    #[test]
    fn target_scoped_override_only_applies_on_matching_target() {
        let directory = tempfile::tempdir().expect("temporary configuration directory");
        let path = directory.path().join("hawk.toml");
        std::fs::write(
            &path,
            r#"
[[override]]
lint = "hawk::dead_public"
crate = "library"
item = "unused"
level = "expect"
target = "cfg(windows)"
reason = "only retained on Windows"
"#,
        )
        .expect("write configuration");
        let config = Config::load(directory.path(), Some(&path)).expect("load configuration");
        let fragments = vec![fragment()];

        let windows = config.apply(
            &target("x86_64-pc-windows-msvc", &["windows"]),
            &fragments,
            &[],
            analyze(&fragments, &[], &candidate_crates(), &HashSet::new()),
        );
        assert!(windows.findings.is_empty());
        assert!(windows.config_diagnostics.is_empty());

        let unix = config.apply(
            &target("aarch64-apple-darwin", &["unix"]),
            &fragments,
            &[],
            analyze(&fragments, &[], &candidate_crates(), &HashSet::new()),
        );
        assert_eq!(unix.findings.len(), 1);
        assert!(unix.config_diagnostics.is_empty());
    }

    #[test]
    fn inapplicable_override_does_not_report_an_unknown_item() {
        let directory = tempfile::tempdir().expect("temporary configuration directory");
        let path = directory.path().join("hawk.toml");
        std::fs::write(
            &path,
            r#"
[[override]]
lint = "hawk::dead_public"
crate = "library"
item = "windows_only_item"
level = "expect"
target = "cfg(windows)"
reason = "only compiled on Windows"
"#,
        )
        .expect("write configuration");
        let config = Config::load(directory.path(), Some(&path)).expect("load configuration");
        let fragments = vec![fragment()];
        let findings = analyze(&fragments, &[], &candidate_crates(), &HashSet::new());

        let applied = config.apply(
            &target("aarch64-apple-darwin", &["unix"]),
            &fragments,
            &[],
            findings,
        );

        assert_eq!(applied.findings.len(), 1);
        assert!(applied.config_diagnostics.is_empty());
    }

    #[test]
    fn target_scoped_production_consumer_only_applies_on_matching_target() {
        let directory = tempfile::tempdir().expect("temporary configuration directory");
        let path = directory.path().join("hawk.toml");
        std::fs::write(
            &path,
            r#"
[[production]]
package = "windows-runner"
bin = "windows-runner"
target = "cfg(windows)"
reason = "shipped on Windows"
"#,
        )
        .expect("write configuration");
        let config = Config::load(directory.path(), Some(&path)).expect("load configuration");

        let windows = config
            .production_consumers(&target("x86_64-pc-windows-msvc", &["windows"]))
            .collect::<Vec<_>>();
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].package, "windows-runner");
        assert_eq!(windows[0].binary, "windows-runner");

        assert_eq!(
            config
                .production_consumers(&target("aarch64-apple-darwin", &["unix"]))
                .count(),
            0
        );
    }
}
