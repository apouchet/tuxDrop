//! Trusted devices, whose transfers are accepted without confirmation.
//!
//! The store is re-read on every lookup so the CLI and the daemon can share
//! the file without coordinating; it only ever holds a handful of entries.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::config::save_json;
use crate::discovery::designates;
use crate::identity::same_fingerprint;
use crate::names::Names;

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

    /// The trusted device `target` designates: its stored fingerprint as
    /// written, or as for nearby devices ([`designates`]). Errors when
    /// several match.
    pub fn find(&self, target: &str, names: &Names) -> Result<Option<TrustedDevice>> {
        let devices = self.list()?;
        if let Some(d) = devices
            .iter()
            .find(|d| same_fingerprint(&d.fingerprint, target))
        {
            return Ok(Some(d.clone()));
        }
        let mut found = devices
            .into_iter()
            .filter(|d| designates(target, &d.alias, &d.fingerprint, names.get(&d.fingerprint)));
        let first = found.next();
        if found.next().is_some() {
            bail!("{target:?} matches several trusted devices, give more of the fingerprint");
        }
        Ok(first)
    }

    pub fn add(&self, fingerprint: &str, alias: &str) -> Result<()> {
        let mut devices = self.list()?;
        devices.retain(|d| !same_fingerprint(&d.fingerprint, fingerprint));
        devices.push(TrustedDevice {
            fingerprint: fingerprint.to_ascii_uppercase(),
            alias: alias.to_owned(),
        });
        save_json(&self.path, &devices)
    }

    /// Returns whether an entry was removed.
    pub fn remove(&self, fingerprint: &str) -> Result<bool> {
        let mut devices = self.list()?;
        let before = devices.len();
        devices.retain(|d| !same_fingerprint(&d.fingerprint, fingerprint));
        let removed = devices.len() != before;
        if removed {
            save_json(&self.path, &devices)?;
        }
        Ok(removed)
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

    #[test]
    fn find_by_name_alias_prefix_or_stored_text() {
        let dir = tempfile::tempdir().unwrap();
        let store = TrustStore::new(dir.path().join("trusted.json"));
        store.add("C41ADA01", "fedora").unwrap();
        store.add("C41ADB02", "laptop").unwrap();
        store.add("APOUCHET", "").unwrap();
        let names = crate::names::NameStore::new(dir.path().join("names.json"));
        names.set("c41ada01", "Bureau").unwrap();
        let names = names.load();

        let find = |t| store.find(t, &names).unwrap().map(|d| d.fingerprint);
        assert_eq!(find("bureau").as_deref(), Some("C41ADA01"));
        assert_eq!(find("LAPTOP").as_deref(), Some("C41ADB02"));
        assert_eq!(find("c41ada").as_deref(), Some("C41ADA01"));
        assert_eq!(find("apouchet").as_deref(), Some("APOUCHET"));
        assert_eq!(find("unknown"), None);
        assert!(store.find("C41AD", &names).unwrap().is_none());
        store.add("C41ADA03", "other").unwrap();
        assert!(store.find("c41ada", &names).is_err());
    }
}
