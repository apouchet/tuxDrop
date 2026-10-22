//! Trusted devices, whose transfers are accepted without confirmation.
//!
//! The store is re-read on every lookup so the CLI and the daemon can share
//! the file without coordinating; it only ever holds a handful of entries.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::identity::same_fingerprint;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TrustedDevice {
    pub fingerprint: String,
    pub alias: String,
}

#[derive(Clone, Debug)]
pub struct TrustStore {
    path: PathBuf,
}

impl TrustStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn list(&self) -> Result<Vec<TrustedDevice>> {
        match fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .with_context(|| format!("parsing {}", self.path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(e).with_context(|| format!("reading {}", self.path.display())),
        }
    }

    /// `fingerprint` must come from a TLS-verified certificate, never from a
    /// self-declared JSON field.
    pub fn is_trusted(&self, fingerprint: &str) -> bool {
        match self.list() {
            Ok(devices) => devices
                .iter()
                .any(|d| same_fingerprint(&d.fingerprint, fingerprint)),
            Err(e) => {
                tracing::warn!("trust store unreadable, treating as empty: {e:#}");
                false
            }
        }
    }

    pub fn add(&self, fingerprint: &str, alias: &str) -> Result<()> {
        let mut devices = self.list()?;
        devices.retain(|d| !same_fingerprint(&d.fingerprint, fingerprint));
        devices.push(TrustedDevice {
            fingerprint: fingerprint.to_ascii_uppercase(),
            alias: alias.to_owned(),
        });
        self.save(&devices)
    }

    /// Returns whether an entry was removed.
    pub fn remove(&self, fingerprint: &str) -> Result<bool> {
        let mut devices = self.list()?;
        let before = devices.len();
        devices.retain(|d| !same_fingerprint(&d.fingerprint, fingerprint));
        let removed = devices.len() != before;
        if removed {
            self.save(&devices)?;
        }
        Ok(removed)
    }

    fn save(&self, devices: &[TrustedDevice]) -> Result<()> {
        if let Some(dir) = self.path.parent() {
            fs::create_dir_all(dir)?;
        }
        let tmp = self.path.with_extension("json.tmp");
        fs::write(&tmp, serde_json::to_vec_pretty(devices)?)?;
        fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_lookup_remove_case_insensitive() {
        let dir = tempfile::tempdir().unwrap();
        let store = TrustStore::new(dir.path().join("trusted.json"));
        assert!(!store.is_trusted("ab"));
        store.add("ab", "laptop").unwrap();
        store.add("AB", "laptop renamed").unwrap();
        assert_eq!(store.list().unwrap().len(), 1);
        assert!(store.is_trusted("aB"));
        assert!(store.remove("ab").unwrap());
        assert!(!store.remove("ab").unwrap());
        assert!(!store.is_trusted("AB"));
    }
}
