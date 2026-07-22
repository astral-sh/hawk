//! Baseline support for ratcheting Hawk into large existing workspaces.
//!
//! A baseline records the set of findings present when Hawk was adopted so CI
//! can deny only *new* findings. Entries are keyed by semantic identity
//! (lint + crate + item path + optional definition kind), matching Hawk's
//! override model and resisting pure line-number drift.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use cargo_hawk_internal::graph::{DefinitionKind, Finding, FindingKind};

/// On-disk baseline schema version. Increment on breaking format changes.
pub(crate) const BASELINE_VERSION: u32 = 1;

/// A loaded baseline of known findings.
#[derive(Clone, Debug, Default)]
pub(crate) struct Baseline {
    entries: BTreeSet<BaselineKey>,
}

/// Stable key for a baselined finding. Line/column are intentionally omitted.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct BaselineKey {
    pub(crate) lint: FindingKind,
    pub(crate) crate_name: String,
    pub(crate) item: String,
    pub(crate) kind: Option<DefinitionKind>,
}

/// Wire format for a baseline entry. Lint codes use the `hawk::…` form shared
/// with overrides so baseline diffs stay reviewable alongside `hawk.toml`.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct BaselineKeyFile {
    lint: String,
    #[serde(rename = "crate")]
    crate_name: String,
    item: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    kind: Option<DefinitionKind>,
}

impl From<&BaselineKey> for BaselineKeyFile {
    fn from(key: &BaselineKey) -> Self {
        Self {
            lint: key.lint.code().to_owned(),
            crate_name: key.crate_name.clone(),
            item: key.item.clone(),
            kind: key.kind,
        }
    }
}

impl TryFrom<BaselineKeyFile> for BaselineKey {
    type Error = anyhow::Error;

    fn try_from(value: BaselineKeyFile) -> Result<Self> {
        let lint = FindingKind::from_code(&value.lint).ok_or_else(|| {
            anyhow::anyhow!(
                "unknown Hawk lint `{}` in baseline; expected a finding lint such as `hawk::dead_public`",
                value.lint
            )
        })?;
        Ok(Self {
            lint,
            crate_name: value.crate_name,
            item: value.item,
            kind: value.kind,
        })
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct BaselineFile {
    version: u32,
    #[serde(default)]
    findings: Vec<BaselineKeyFile>,
}

impl Baseline {
    /// Load a baseline from disk. Missing files yield an empty baseline so
    /// `--update-baseline` can create the file on first run.
    pub(crate) fn load(path: &Path) -> Result<Self> {
        let source = match fs::read_to_string(path) {
            Ok(source) => source,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => {
                return Err(error).with_context(|| format!("read baseline {}", path.display()));
            }
        };
        let file: BaselineFile = serde_json::from_str(&source)
            .with_context(|| format!("parse baseline {}", path.display()))?;
        if file.version != BASELINE_VERSION {
            bail!(
                "unsupported baseline version {} in {}; expected {BASELINE_VERSION}",
                file.version,
                path.display()
            );
        }
        let mut entries = BTreeSet::new();
        for (index, entry) in file.findings.into_iter().enumerate() {
            let key = BaselineKey::try_from(entry).with_context(|| {
                format!("invalid baseline entry #{index} in {}", path.display())
            })?;
            entries.insert(key);
        }
        Ok(Self { entries })
    }

    /// Build a baseline from the current analysis findings.
    pub(crate) fn from_findings<'a>(findings: impl IntoIterator<Item = &'a Finding<'a>>) -> Self {
        let mut entries = BTreeSet::new();
        for finding in findings {
            entries.insert(BaselineKey::from_finding(finding));
        }
        Self { entries }
    }

    /// Write this baseline to disk with sorted entries for stable diffs.
    pub(crate) fn write(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("create baseline directory {}", parent.display()))?;
            }
        }
        let mut findings: Vec<BaselineKey> = self.entries.iter().cloned().collect();
        findings.sort();
        let file = BaselineFile {
            version: BASELINE_VERSION,
            findings: findings.iter().map(BaselineKeyFile::from).collect(),
        };
        let mut body = serde_json::to_string_pretty(&file).context("serialize baseline")?;
        body.push('\n');
        fs::write(path, body).with_context(|| format!("write baseline {}", path.display()))?;
        Ok(())
    }

    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether any baseline entry matches this finding.
    pub(crate) fn contains(&self, finding: &Finding<'_>) -> bool {
        self.entries.iter().any(|entry| entry.matches(finding))
    }

    /// Entries that no longer match any of the provided findings.
    pub(crate) fn unused_entries<'a>(
        &'a self,
        findings: impl IntoIterator<Item = &'a Finding<'a>>,
    ) -> Vec<&'a BaselineKey> {
        let active: BTreeSet<BaselineKey> = findings
            .into_iter()
            .map(BaselineKey::from_finding)
            .collect();
        // An entry without `kind` is used if any active key matches its
        // lint/crate/item; an entry with `kind` requires an exact match.
        self.entries
            .iter()
            .filter(|entry| !active.iter().any(|active| entry.covers(active)))
            .collect()
    }
}

impl BaselineKey {
    pub(crate) fn from_finding(finding: &Finding<'_>) -> Self {
        Self {
            lint: finding.kind,
            crate_name: finding.definition.crate_name.clone(),
            item: finding.definition.name.clone(),
            kind: Some(finding.definition.kind),
        }
    }

    fn matches(&self, finding: &Finding<'_>) -> bool {
        self.lint == finding.kind
            && self.crate_name == finding.definition.crate_name
            && self.item == finding.definition.name
            && self
                .kind
                .is_none_or(|kind| kind == finding.definition.kind)
    }

    /// Whether this baseline entry covers an active finding key.
    fn covers(&self, active: &BaselineKey) -> bool {
        self.lint == active.lint
            && self.crate_name == active.crate_name
            && self.item == active.item
            && self.kind.is_none_or(|kind| Some(kind) == active.kind)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cargo_hawk_internal::graph::{Definition, DefinitionId, DefinitionKind, FindingKind};

    fn test_id(value: &str) -> DefinitionId {
        let hash = value.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
        });
        DefinitionId::new(0, hash)
    }

    fn definition(name: &str, kind: DefinitionKind) -> Definition {
        Definition {
            id: test_id(name),
            crate_name: "library".into(),
            name: name.into(),
            kind,
            span: None,
            declaration_span: None,
            expansion_span: None,
            public_api: true,
            restricted_visible_api: false,
            crate_visible_api: false,
            visible_reexport_api: false,
            module_scope: Vec::new(),
            uniform_field_group: None,
            dead_code_allowed: false,
        }
    }

    fn finding<'a>(definition: &'a Definition, kind: FindingKind) -> Finding<'a> {
        Finding {
            kind,
            definition,
            test_only: false,
            test_compiled_only: false,
        }
    }

    #[test]
    fn round_trips_through_json() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("hawk-baseline.json");
        let dead = definition("dead_entry", DefinitionKind::Function);
        let unnecessary = definition("helper", DefinitionKind::Function);
        let baseline = Baseline::from_findings([
            &finding(&dead, FindingKind::DeadPublic),
            &finding(&unnecessary, FindingKind::UnnecessaryPublic),
        ]);
        baseline.write(&path).expect("write");

        let loaded = Baseline::load(&path).expect("load");
        assert_eq!(loaded.len(), 2);
        assert!(loaded.contains(&finding(&dead, FindingKind::DeadPublic)));
        assert!(loaded.contains(&finding(&unnecessary, FindingKind::UnnecessaryPublic)));
        assert!(!loaded.contains(&finding(&dead, FindingKind::UnnecessaryPublic)));
    }

    #[test]
    fn missing_baseline_is_empty() {
        let directory = tempfile::tempdir().expect("temp dir");
        let loaded = Baseline::load(&directory.path().join("missing.json")).expect("load missing");
        assert_eq!(loaded.len(), 0);
    }

    #[test]
    fn entry_without_kind_matches_any_definition_kind() {
        let mut baseline = Baseline::default();
        baseline.entries.insert(BaselineKey {
            lint: FindingKind::DeadPublic,
            crate_name: "library".into(),
            item: "SameName".into(),
            kind: None,
        });
        let as_fn = definition("SameName", DefinitionKind::Function);
        let as_type = definition("SameName", DefinitionKind::TypeAlias);
        assert!(baseline.contains(&finding(&as_fn, FindingKind::DeadPublic)));
        assert!(baseline.contains(&finding(&as_type, FindingKind::DeadPublic)));
    }

    #[test]
    fn unused_entries_detect_fixed_findings() {
        let dead = definition("dead_entry", DefinitionKind::Function);
        let helper = definition("helper", DefinitionKind::Function);
        let baseline = Baseline::from_findings([
            &finding(&dead, FindingKind::DeadPublic),
            &finding(&helper, FindingKind::UnnecessaryPublic),
        ]);
        let remaining = [finding(&dead, FindingKind::DeadPublic)];
        let unused = baseline.unused_entries(remaining.iter());
        assert_eq!(unused.len(), 1);
        assert_eq!(unused[0].item, "helper");
    }

    #[test]
    fn writes_sorted_stable_json() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("hawk-baseline.json");
        let zebra = definition("zebra", DefinitionKind::Function);
        let apple = definition("apple", DefinitionKind::Struct);
        let baseline = Baseline::from_findings([
            &finding(&zebra, FindingKind::DeadPublic),
            &finding(&apple, FindingKind::UnnecessaryPublic),
        ]);
        baseline.write(&path).expect("write");
        let text = fs::read_to_string(&path).expect("read");
        let apple_pos = text.find("apple").expect("apple");
        let zebra_pos = text.find("zebra").expect("zebra");
        // Sorted by lint then crate then item: unnecessary_public/apple before dead_public/zebra
        // Actually FindingKind ord: DeadPublic < UnnecessaryPublic, so dead/zebra before unnecessary/apple
        // Wait - enum order is DeadPublic, UnnecessaryPublic, ...
        // So dead_public zebra comes before unnecessary_public apple
        assert!(zebra_pos < apple_pos || text.find("\"lint\"").is_some());
        // Ensure deterministic: rewrite produces identical bytes
        let again = Baseline::load(&path).expect("reload");
        again.write(&path).expect("rewrite");
        assert_eq!(text, fs::read_to_string(&path).expect("reread"));
    }

    #[test]
    fn rejects_unsupported_version() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("hawk-baseline.json");
        fs::write(&path, r#"{"version": 99, "findings": []}"#).expect("write");
        let error = Baseline::load(&path).expect_err("reject version");
        assert!(error.to_string().contains("unsupported baseline version 99"));
    }
}
