//! User configuration (`~/.config/tuxdrop/config.toml`) and data paths.

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::proto::DEFAULT_PORT;

#[derive(Deserialize, Debug, Clone)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub alias: String,
    pub download_dir: PathBuf,
    pub port: u16,
    /// Required from senders on `prepare-upload` when set.
    pub pin: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        let alias = rustix::system::uname()
            .nodename()
            .to_string_lossy()
            .into_owned();
        let download_dir = dirs::download_dir()
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| PathBuf::from("."))
            .join("tuxDrop");
        Self {
            alias,
            download_dir,
            port: DEFAULT_PORT,
            pin: None,
        }
    }
}

impl Config {
    pub fn load() -> Result<Self> {
        let path = config_dir().join("config.toml");
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }
}

pub fn config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("tuxdrop")
}

/// Holds the certificate, private key and trusted devices.
pub fn data_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("tuxdrop")
}
