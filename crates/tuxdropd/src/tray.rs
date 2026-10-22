//! System tray icon (StatusNotifierItem) listing nearby devices; each one
//! has a submenu to name it and to trust it.
//!
//! GNOME shows it only with the AppIndicator extension; without a tray host
//! the icon waits for one and the daemon works as before.

use std::sync::Arc;
use std::time::Duration;

use ksni::menu::{CheckmarkItem, StandardItem, SubMenu};
use ksni::{MenuItem, OfflineReason, Status, TrayMethods};
use tokio::process::Command;
use tuxdrop_core::Node;
use tuxdrop_core::identity::same_fingerprint;

use crate::notifications::{ACCEPT, ALWAYS, REJECT, escape};
use crate::{Shared, sender_label};

/// How often expired devices are dropped from the menu when no new one shows up.
const EXPIRY_CHECK: Duration = Duration::from_secs(30);

/// A nearby device as the menu shows it.
#[derive(Clone, PartialEq)]
struct Device {
    fingerprint: String,
    alias: String,
    name: Option<String>,
    /// `Name (alias)` or the alias.
    label: String,
    trusted: bool,
}

impl Device {
    fn menu_item(&self) -> MenuItem<Tray> {
        let mut label = self.label.clone();
        if self.trusted {
            label.push_str("  · trusted");
        }
        let rename = self.clone();
        let trust = self.clone();
        let short = &self.fingerprint[..self.fingerprint.len().min(16)];
        SubMenu {
            // A single underscore marks a mnemonic in menu labels.
            label: label.replace('_', "__"),
            submenu: vec![
                StandardItem {
                    label: format!("Fingerprint {short}…"),
                    enabled: false,
                    ..Default::default()
                }
                .into(),
                StandardItem {
                    label: "Rename…".into(),
                    icon_name: "document-edit-symbolic".into(),
                    activate: Box::new(move |tray: &mut Tray| {
                        tokio::spawn(rename_dialog(tray.shared.clone(), rename.clone()));
                    }),
                    ..Default::default()
                }
                .into(),
                CheckmarkItem {
                    label: "Trusted (accept without asking)".into(),
                    checked: self.trusted,
                    activate: Box::new(move |tray: &mut Tray| tray.toggle_trust(&trust)),
                    ..Default::default()
                }
                .into(),
            ],
            ..Default::default()
        }
        .into()
    }
}

/// An incoming transfer waiting for the user's decision.
#[derive(Clone, PartialEq)]
struct Request {
    id: String,
    label: String,
    verified: bool,
}

impl Request {
    fn menu_items(&self) -> Vec<MenuItem<Tray>> {
        let mut items = vec![
            StandardItem {
                label: self.label.replace('_', "__"),
                icon_name: "mail-unread-symbolic".into(),
                enabled: false,
                ..Default::default()
            }
            .into(),
            self.choice("Accept", ACCEPT),
            self.choice("Decline", REJECT),
        ];
        if self.verified {
            items.push(self.choice("Always accept from this device", ALWAYS));
        }
        items
    }

    fn choice(&self, label: &str, choice: &'static str) -> MenuItem<Tray> {
        let id = self.id.clone();
        StandardItem {
            label: label.into(),
            activate: Box::new(move |tray: &mut Tray| {
                let shared = tray.shared.clone();
                let id = id.clone();
                tokio::spawn(async move { shared.decide(&id, choice).await });
            }),
            ..Default::default()
        }
        .into()
    }
}

struct Tray {
    shared: Arc<Shared>,
    /// Sorted by label.
    devices: Vec<Device>,
    requests: Vec<Request>,
}

impl Tray {
    fn toggle_trust(&mut self, device: &Device) {
        let store = Node::trust_store();
        let result = if device.trusted {
            store.remove(&device.fingerprint).map(drop)
        } else {
            store.add(&device.fingerprint, &device.alias)
        };
        if let Err(e) = result {
            tracing::error!("cannot update trusted devices: {e:#}");
        }
        self.devices = devices(&self.shared);
    }
}

impl ksni::Tray for Tray {
    const MENU_ON_ACTIVATE: bool = true;

    fn id(&self) -> String {
        "tuxdrop".into()
    }

    fn title(&self) -> String {
        "tuxDrop".into()
    }

    fn icon_name(&self) -> String {
        "send-to-symbolic".into()
    }

    fn attention_icon_name(&self) -> String {
        "mail-unread-symbolic".into()
    }

    /// Asks for attention while a transfer waits for a decision.
    fn status(&self) -> Status {
        if self.requests.is_empty() {
            Status::Active
        } else {
            Status::NeedsAttention
        }
    }

    /// Picks up names and trust changed from the CLI; new devices appear in
    /// the open menu as they answer.
    fn menu_about_to_show(&mut self) {
        self.devices = devices(&self.shared);
        self.shared.announce();
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let mut items = Vec::new();
        for request in &self.requests {
            items.extend(request.menu_items());
            items.push(MenuItem::Separator);
        }
        if self.devices.is_empty() {
            items.push(
                StandardItem {
                    label: "No device found".into(),
                    enabled: false,
                    ..Default::default()
                }
                .into(),
            );
        } else {
            items.extend(self.devices.iter().map(Device::menu_item));
        }
        items.push(MenuItem::Separator);
        items.push(
            StandardItem {
                label: "Open received files".into(),
                icon_name: "folder-download-symbolic".into(),
                activate: Box::new(|tray: &mut Self| {
                    let shared = tray.shared.clone();
                    tokio::spawn(async move {
                        let dir = &shared.node.config.download_dir;
                        // The file manager shows an error for a missing folder.
                        if let Err(e) = std::fs::create_dir_all(dir) {
                            tracing::warn!("cannot create {}: {e}", dir.display());
                        }
                        shared.file_manager.open_folder(dir).await;
                    });
                }),
                ..Default::default()
            }
            .into(),
        );
        items.push(
            StandardItem {
                label: "Refresh".into(),
                icon_name: "view-refresh-symbolic".into(),
                activate: Box::new(|tray: &mut Self| tray.shared.announce()),
                ..Default::default()
            }
            .into(),
        );
        items
    }

    fn watcher_offline(&self, reason: OfflineReason) -> bool {
        tracing::info!(
            "no system tray ({reason:?}); on GNOME, enable the AppIndicator extension to see the icon"
        );
        true
    }
}

/// The devices currently known to the daemon, with their name and trust.
fn devices(shared: &Shared) -> Vec<Device> {
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

/// Asks for a new name with zenity; a blank one goes back to the alias.
async fn rename_dialog(shared: Arc<Shared>, device: Device) {
    let output = Command::new("zenity")
        .args(["--entry", "--title=tuxDrop"])
        .arg(format!(
            "--text=Name for {} (empty: use its own name)",
            escape(&device.alias)
        ))
        .arg(format!(
            "--entry-text={}",
            device.name.as_deref().unwrap_or(&device.alias)
        ))
        .output()
        .await;
    let output = match output {
        Ok(output) => output,
        Err(e) => {
            tracing::warn!("cannot run zenity: {e}");
            shared
                .notifier
                .inform(
                    0,
                    "Cannot rename",
                    "Renaming needs zenity (sudo apt install zenity / sudo dnf install zenity)",
                )
                .await;
            return;
        }
    };
    if !output.status.success() {
        return; // cancelled
    }
    let name = String::from_utf8_lossy(&output.stdout);
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

/// Shows the tray icon and keeps its device list up to date.
pub async fn run(shared: Arc<Shared>) {
    let tray = Tray {
        shared: shared.clone(),
        devices: Vec::new(),
        requests: Vec::new(),
    };
    // Waits for a tray host instead of failing when the session has none yet
    // (daemon started before the shell, extension enabled later).
    let handle = match tray.assume_sni_available(true).spawn().await {
        Ok(handle) => handle,
        Err(e) => {
            tracing::warn!("cannot create the tray icon: {e}");
            return;
        }
    };
    let mut shown = (Vec::new(), Vec::new());
    loop {
        let current = (devices(&shared), requests(&shared));
        if current != shown {
            shown = current.clone();
            let (devices, requests) = current;
            let update = |tray: &mut Tray| {
                tray.devices = devices;
                tray.requests = requests;
            };
            if handle.update(update).await.is_none() {
                return;
            }
        }
        tokio::select! {
            _ = shared.tray_changed.notified() => {}
            _ = tokio::time::sleep(EXPIRY_CHECK) => {}
        }
    }
}
