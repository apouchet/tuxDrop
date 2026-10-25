//! Windows: prepared, not finished. The notification area icon, the control
//! API and Explorer work; requests are answered from the icon's menu or with
//! `tuxdrop accept`.
//!
//! To do: toast notifications with buttons (WinRT `ToastNotificationManager`,
//! needs an AppUserModelID), text and file dialogs (`IFileOpenDialog`), the
//! clipboard (`SetClipboardData`), and an Explorer context menu entry.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use tokio::process::Command;
use tokio::sync::mpsc;

use crate::notifications::NotificationEvent;

pub const CLIPBOARD_HELP: &str = "Copying is not available on Windows yet";
pub const DIALOG_HELP: &str = "Dialogs are not available on Windows yet";

/// Logs what the other platforms show as notifications.
pub struct Notifier {
    _events: mpsc::UnboundedSender<NotificationEvent>,
}

impl Notifier {
    pub fn new(events: mpsc::UnboundedSender<NotificationEvent>) -> Self {
        Self { _events: events }
    }

    pub async fn ask(&self, _summary: &str, _body: &str, _offer_always: bool) -> Result<u32> {
        bail!("notifications are not available on Windows yet, see the tray icon")
    }

    pub async fn transfer(&self, _replaces: u32, summary: &str, body: &str, percent: u32) -> u32 {
        if percent == 0 {
            tracing::info!("{summary}: {body}");
        }
        0
    }

    pub async fn conclude(
        &self,
        _replaces: u32,
        summary: &str,
        body: &str,
        _action: Option<[&str; 2]>,
    ) -> u32 {
        tracing::info!("{summary}: {body}");
        0
    }

    pub async fn inform(&self, _replaces: u32, summary: &str, body: &str) -> u32 {
        tracing::info!("{summary}: {body}");
        0
    }

    pub async fn close(&self, _id: u32) {}
}

/// A blocking alert, for when the daemon stops.
pub fn alert(summary: &str, body: &str) {
    tracing::error!("{summary}: {body}");
}

/// Starting at login: not yet (a Run registry value or a Startup folder
/// shortcut).
pub mod login {
    pub fn enabled() -> Option<bool> {
        None
    }

    pub fn set(_enabled: bool) -> anyhow::Result<()> {
        anyhow::bail!("not available on Windows yet")
    }
}

/// Integration with the shell: nothing yet.
pub async fn integrate() {}

/// Explorer.
pub struct FileManager;

impl FileManager {
    /// Opens `folder`.
    pub async fn open_folder(&self, folder: &Path) {
        explorer(&[folder.as_os_str().to_owned()]).await;
    }

    /// Opens the folder holding the first of `items`, with it selected.
    pub async fn select(&self, items: &[impl AsRef<Path>]) {
        if let Some(first) = items.first() {
            let mut arg = std::ffi::OsString::from("/select,");
            arg.push(first.as_ref());
            explorer(&[arg]).await;
        }
    }
}

async fn explorer(args: &[std::ffi::OsString]) {
    // Explorer exits with 1 even when it succeeds.
    if let Err(e) = Command::new("explorer").args(args).status().await {
        tracing::warn!("cannot run explorer: {e}");
    }
}

pub async fn copy_to_clipboard(_text: &str) -> Result<()> {
    bail!("{CLIPBOARD_HELP}")
}

pub async fn ask_text(_prompt: &str, _initial: &str) -> Result<Option<String>> {
    bail!("{DIALOG_HELP}")
}

pub async fn pick_paths(_title: &str, _folders: bool) -> Result<Option<Vec<PathBuf>>> {
    bail!("{DIALOG_HELP}")
}

pub async fn pick_device(_prompt: &str, _labels: &[String]) -> Result<Option<usize>> {
    bail!("{DIALOG_HELP}")
}
