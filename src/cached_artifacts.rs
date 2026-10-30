use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, BufReader, Write as _};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use cargo_hawk_internal::fragment_cache::{Entry, fingerprint};
use cargo_hawk_internal::graph::CollectionOptions;
use cargo_hawk_internal::protocol;
use cargo_metadata::{Message, PackageId, diagnostic::DiagnosticLevel};

pub(crate) struct CachedArtifacts {
    pub(crate) directory: PathBuf,
    pub(crate) run_id: String,
    members: HashSet<PackageId>,
}

impl CachedArtifacts {
    pub(crate) fn new(
        target: &Path,
        driver: &Path,
        options: CollectionOptions,
        members: &[PackageId],
    ) -> Result<Self> {
        let driver = fs::read(driver).context("fingerprint Hawk compiler driver")?;
        let generation = fingerprint((protocol::VERSION, &driver, options.as_env_value()));
        let run_id = format!("hawk-cache-{generation:016x}");
        let directory = target.join("hawk-fragments").join(&run_id);
        fs::create_dir_all(&directory).context("create Hawk fragment cache")?;
        Ok(Self {
            directory,
            run_id,
            members: members.iter().cloned().collect(),
        })
    }

    /// Cargo replays compiler messages for fresh units. Require one reference
    /// per workspace artifact before accepting this invocation's active graph.
    pub(crate) fn read_messages(
        &self,
        file: File,
        color: anstream::ColorChoice,
        show_output: bool,
        text_on_stdout: bool,
    ) -> Result<Option<BTreeSet<Entry>>> {
        let mut stderr = anstream::AutoStream::new(std::io::stderr(), color);
        let mut balance = HashMap::new();
        let mut entries = BTreeSet::new();
        let mut finished = false;
        for message in Message::parse_stream(BufReader::new(file)) {
            match message? {
                Message::CompilerArtifact(artifact)
                    if self.members.contains(&artifact.package_id) =>
                {
                    *balance
                        .entry((artifact.package_id, artifact.target))
                        .or_insert(0_i64) += 1;
                }
                Message::CompilerMessage(message) => {
                    let diagnostic = message.message;
                    if self.members.contains(&message.package_id)
                        && diagnostic.level == DiagnosticLevel::Note
                        && diagnostic.spans.is_empty()
                        && let Some(entry) = Entry::from_note(&diagnostic.message)
                    {
                        entries.insert(entry);
                        *balance
                            .entry((message.package_id, message.target))
                            .or_insert(0) -= 1;
                    } else if show_output && let Some(rendered) = diagnostic.rendered {
                        stderr
                            .write_all(rendered.as_bytes())
                            .or_else(ignore_closed_stderr)?;
                    }
                }
                Message::BuildFinished(result) => finished = result.success,
                Message::TextLine(line) if show_output => {
                    if text_on_stdout {
                        writeln!(std::io::stdout(), "{line}")?;
                    } else {
                        writeln!(stderr, "{line}").or_else(ignore_closed_stderr)?;
                    }
                }
                _ => {}
            }
        }
        Ok((finished && balance.values().all(|count| *count == 0)).then_some(entries))
    }
}

fn ignore_closed_stderr(error: io::Error) -> io::Result<()> {
    if error.kind() == io::ErrorKind::BrokenPipe {
        Ok(())
    } else {
        Err(error)
    }
}
