//! Graph references that Cargo replays with a workspace unit's compiler messages.

use std::fs;
use std::hash::{Hash, Hasher};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use rustc_hash::FxHasher;

use crate::graph::Fragment;

const NOTE_PREFIX: &str = "hawk::cached_fragment ";

pub fn fingerprint(value: impl Hash) -> u64 {
    let mut hasher = FxHasher::default();
    value.hash(&mut hasher);
    hasher.finish()
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Entry {
    unit: u64,
    contents: u64,
}

impl Entry {
    pub fn write(directory: &Path, unit: u64, fragment: &Fragment) -> io::Result<Self> {
        let bytes = serde_json::to_vec(fragment)?;
        let entry = Self {
            unit,
            contents: fingerprint(&bytes),
        };
        let mut file = tempfile::NamedTempFile::new_in(directory)?;
        file.write_all(&bytes)?;
        file.persist(entry.path(directory))?;
        Ok(entry)
    }

    pub fn note(self) -> String {
        format!("{NOTE_PREFIX}{:016x} {:016x}", self.unit, self.contents)
    }

    pub fn from_note(note: &str) -> Option<Self> {
        let (unit, contents) = note.strip_prefix(NOTE_PREFIX)?.split_once(' ')?;
        if unit.len() != 16 || contents.len() != 16 {
            return None;
        }
        Some(Self {
            unit: u64::from_str_radix(unit, 16).ok()?,
            contents: u64::from_str_radix(contents, 16).ok()?,
        })
    }

    pub fn read(self, directory: &Path) -> Option<Fragment> {
        let bytes = fs::read(self.path(directory)).ok()?;
        if fingerprint(&bytes) != self.contents {
            return None;
        }
        serde_json::from_slice(&bytes).ok()
    }

    fn path(self, directory: &Path) -> PathBuf {
        directory.join(format!("{:016x}.json", self.unit))
    }
}
