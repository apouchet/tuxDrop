//! What the tray icon's menu shows and what its entries do, whatever draws
//! it (`platform::tray`): pending requests, transfers in progress, and the
//! nearby devices, each with a submenu to send to it, name it and trust it.

use std::sync::Arc;
use std::time::Duration;

use tuxdrop_core::Node;
use tuxdrop_core::identity::same_fingerprint;

use crate::{Shared, platform, sender_label};

/// How often expired devices are dropped from the menu when no new one shows up.
const EXPIRY_CHECK: Duration = Duration::from_secs(30);

/// A nearby device as the menu shows it.
#[derive(Clone, PartialEq)]
pub struct Device {
    pub fingerprint: String,
    pub alias: String,
    pub name: Option<String>,
    /// `Name (alias)` or the alias.
    pub label: String,
    pub trusted: bool,
}

impl Device {
    /// The label, marked when the device is trusted.
    pub fn menu_label(&self) -> String {
        let mut label = self.label.clone();
        if self.trusted {
            label.push_str("  · trusted");
        }
        label
    }

    pub fn short_fingerprint(&self) -> String {
        let short = &self.fingerprint[..self.fingerprint.len().min(16)];
        format!("Fingerprint {short}…")
    }
}

/// An incoming transfer waiting for the user's decision.
#[derive(Clone, PartialEq)]
pub struct Request {
    pub id: String,
    pub label: String,
    pub verified: bool,
}

/// A transfer in progress, in either direction.
#[derive(Clone, PartialEq)]
pub struct Transfer {
    pub id: String,
    /// "Receiving from … · 45%".
    pub label: String,
}

/// Everything the menu shows.
#[derive(Clone, Default, PartialEq)]
pub struct Snapshot {
    /// Sorted by label.
    pub devices: Vec<Device>,
    pub requests: Vec<Request>,
    pub transfers: Vec<Transfer>,
}

impl Snapshot {
    pub fn take(shared: &Shared) -> Self {
        Self {
            devices: devices(shared),
            requests: requests(shared),
            transfers: transfers(shared),
        }
    }
}

/// The devices currently known to the daemon, with their name and trust.
pub fn devices(shared: &Shared) -> Vec<Device> {
    let names = Node::name_store().load();
    let trusted = Node::trust_store().list().unwrap_or_else(|e| {
        tracing::warn!("trust store unreadable: {e:#}");
        Vec::new()
    });
    let mut devices: Vec<Device> = shared
        .live_peers()
        .into_iter()
        .map(|(peer, _)| {
            let info = peer.info;
            Device {
                trusted: trusted
                    .iter()
                    .any(|t| same_fingerprint(&t.fingerprint, &info.fingerprint)),
                name: names.get(&info.fingerprint).map(str::to_owned),
                label: names.label(&info.alias, &info.fingerprint),
                fingerprint: info.fingerprint,
                alias: info.alias,
            }
        })
        .collect();
    devices.sort_by_key(|d| d.label.to_lowercase());
    devices
}

/// The incoming transfers waiting for a decision (at most one: the receiver
/// handles one request at a time).
fn requests(shared: &Shared) -> Vec<Request> {
    shared
        .pending
        .lock()
        .unwrap()
        .iter()
        .map(|(id, p)| {
            let r = &p.request;
            Request {
                id: id.clone(),
                label: format!(
                    "{} wants to send {} file(s)",
                    sender_label(r),
                    r.files.len()
                ),
                verified: r.is_verified(),
            }
        })
        .collect()
}

/// Transfers in progress, with the percentage last shown in notifications.
fn transfers(shared: &Shared) -> Vec<Transfer> {
    let mut transfers: Vec<Transfer> = shared
        .transfers
        .lock()
        .unwrap()
        .iter()
        .map(|(id, t)| Transfer {
            id: id.clone(),
            label: format!("{} · {}%", t.summary, t.shown_percent),
        })
        .collect();
    transfers.sort_by(|a, b| a.label.cmp(&b.label));
    transfers
}

/// Trusts the device, or stops trusting it.
pub fn toggle_trust(shared: &Shared, device: &Device) {
    let store = Node::trust_store();
    let result = if device.trusted {
        store.remove(&device.fingerprint).map(drop)
    } else {
        store.add(&device.fingerprint, &device.alias)
    };
    if let Err(e) = result {
        tracing::error!("cannot update trusted devices: {e:#}");
    }
    shared.tray_changed.notify_one();
}

/// Asks for a new name; a blank one, or the alias, goes back to the alias.
pub async fn rename(shared: Arc<Shared>, device: Device) {
    let prompt = format!("Name for {} (empty: use its own name)", device.alias);
    let current = device.name.as_deref().unwrap_or(&device.alias);
    let name = match platform::ask_text(&prompt, current).await {
        Ok(Some(name)) => name,
        Ok(None) => return, // cancelled
        Err(e) => {
            tracing::warn!("cannot ask for a name: {e:#}");
            shared
                .notifier
                .inform(0, "Cannot rename", platform::DIALOG_HELP)
                .await;
            return;
        }
    };
    let name = if name.trim() == device.alias {
        ""
    } else {
        &name
    };
    match Node::name_store().set(&device.fingerprint, name) {
        Ok(_) => shared.tray_changed.notify_one(),
        Err(e) => tracing::error!("cannot save the device name: {e:#}"),
    }
}

/// Lets the user pick files (or one or more folders) and sends them to
/// `device`; failures are reported by [`Shared::send`].
pub async fn send_to(shared: Arc<Shared>, device: Device, folders: bool) {
    let paths = match platform::pick_paths(&format!("Send to {}", device.label), folders).await {
        Ok(Some(paths)) if !paths.is_empty() => paths,
        Ok(_) => return, // cancelled
        Err(e) => {
            tracing::warn!("cannot ask for files: {e:#}");
            shared
                .notifier
                .inform(0, "Cannot choose files", platform::DIALOG_HELP)
                .await;
            return;
        }
    };
    let _ = shared.send(&device.fingerprint, paths).await;
}

/// Opens the download folder.
pub async fn open_received(shared: Arc<Shared>) {
    let dir = &shared.node.config.download_dir;
    // The file manager shows an error for a missing folder.
    if let Err(e) = std::fs::create_dir_all(dir) {
        tracing::warn!("cannot create {}: {e}", dir.display());
    }
    shared.file_manager.open_folder(dir).await;
}

/// Calls `show` with what the menu must show each time it changes, until
/// `show` returns false.
pub async fn watch<F, Fut>(shared: &Shared, mut show: F)
where
    F: FnMut(Snapshot) -> Fut,
    Fut: Future<Output = bool>,
{
    let mut shown = None;
    loop {
        let current = Snapshot::take(shared);
        if shown.as_ref() != Some(&current) {
            shown = Some(current.clone());
            if !show(current).await {
                return;
            }
        }
        tokio::select! {
            _ = shared.tray_changed.notified() => {}
            _ = tokio::time::sleep(EXPIRY_CHECK) => {}
        }
    }
}
