//! Names given locally to devices, shown alongside their announced alias.
//!
//! Like the trust store, the file is re-read on every lookup so the CLI and
//! the daemon can share it. Names never leave this machine.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::config::save_json;

/// Longer names are truncated.
pub const MAX_NAME_CHARS: usize = 64;

#[derive(Clone, Debug)]
pub struct NameStore {
    path: PathBuf,
}

/// The names at one point in time, keyed by upper-case fingerprint.
#[derive(Clone, Debug, Default)]
pub struct Names(HashMap<String, String>);

impl Names {
    pub fn get(&self, fingerprint: &str) -> Option<&str> {
        self.0
            .get(&fingerprint.to_ascii_uppercase())
            .map(String::as_str)
    }

    /// `Name (alias)` for a named device, its alias otherwise.
    pub fn label(&self, alias: &str, fingerprint: &str) -> String {
        match self.get(fingerprint) {
            Some(name) if alias.is_empty() || name == alias => name.to_owned(),
            Some(name) => format!("{name} ({alias})"),
            None => alias.to_owned(),
        }
    }
}

impl NameStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// An unreadable file counts as empty.
    pub fn load(&self) -> Names {
        self.read().unwrap_or_else(|e| {
            tracing::warn!("device names unreadable, ignoring them: {e:#}");
            Names::default()
        })
    }

    /// Names the device, or forgets its name when `name` is blank; returns
    /// the name stored, without control characters and at most
    /// [`MAX_NAME_CHARS`] long.
    pub fn set(&self, fingerprint: &str, name: &str) -> Result<Option<String>> {
        let Names(mut names) = self.read()?;
        let key = fingerprint.to_ascii_uppercase();
        let name: String = name
            .chars()
            .filter(|c| !c.is_control())
            .collect::<String>()
            .trim()
            .chars()
            .take(MAX_NAME_CHARS)
            .collect();
        let name = name.trim_end().to_owned();
        if name.is_empty() {
            names.remove(&key);
        } else {
            names.insert(key, name.clone());
        }
        save_json(&self.path, &names)?;
        Ok(Some(name).filter(|n| !n.is_empty()))
    }

    fn read(&self) -> Result<Names> {
        match fs::read(&self.path) {
            Ok(bytes) => {
                let names: HashMap<String, String> = serde_json::from_slice(&bytes)
                    .with_context(|| format!("parsing {}", self.path.display()))?;
                Ok(Names(
                    names
                        .into_iter()
                        .map(|(fp, name)| (fp.to_ascii_uppercase(), name))
                        .collect(),
                ))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Names::default()),
            Err(e) => Err(e).with_context(|| format!("reading {}", self.path.display())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_cleans_and_blank_removes() {
        let dir = tempfile::tempdir().unwrap();
        let store = NameStore::new(dir.path().join("names.json"));
        assert_eq!(store.load().get("ab"), None);

        let stored = store.set("ab", "  Bureau\n\u{7}d'Anne  ").unwrap();
        assert_eq!(stored.as_deref(), Some("Bureaud'Anne"));
        assert_eq!(store.load().get("AB"), Some("Bureaud'Anne"));
        assert_eq!(store.load().label("fedora", "aB"), "Bureaud'Anne (fedora)");

        let long = "é".repeat(MAX_NAME_CHARS + 10);
        let stored = store.set("AB", &long).unwrap().unwrap();
        assert_eq!(stored.chars().count(), MAX_NAME_CHARS);

        assert_eq!(store.set("ab", " \t ").unwrap(), None);
        assert_eq!(store.load().get("AB"), None);
        assert_eq!(store.load().label("fedora", "AB"), "fedora");
    }
}
