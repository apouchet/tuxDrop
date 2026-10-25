//! System tray icon (StatusNotifierItem) showing the menu of [`crate::tray`].
//!
//! GNOME shows it only with the AppIndicator extension; without a tray host
//! the icon waits for one and the daemon works as before.

use std::sync::Arc;

use ksni::menu::{CheckmarkItem, StandardItem, SubMenu};
use ksni::{MenuItem, OfflineReason, Status, TrayMethods};

use crate::Shared;
use crate::notifications::{ACCEPT, ALWAYS, REJECT};
use crate::tray::{self, Device, Request, Snapshot, Transfer};

/// A single underscore marks a mnemonic in menu labels.
fn label(text: &str) -> String {
    text.replace('_', "__")
}

fn device_item(device: &Device) -> MenuItem<Tray> {
    let send_files = device.clone();
    let send_folder = device.clone();
    let rename = device.clone();
    let trust = device.clone();
    SubMenu {
        label: label(&device.menu_label()),
        submenu: vec![
            StandardItem {
                label: device.short_fingerprint(),
                enabled: false,
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Send files…".into(),
                icon_name: "document-send-symbolic".into(),
                activate: Box::new(move |tray: &mut Tray| {
                    tokio::spawn(tray::send_to(
                        tray.shared.clone(),
                        send_files.clone(),
                        false,
                    ));
                }),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Send folder…".into(),
                icon_name: "folder-symbolic".into(),
                activate: Box::new(move |tray: &mut Tray| {
                    tokio::spawn(tray::send_to(
                        tray.shared.clone(),
                        send_folder.clone(),
                        true,
                    ));
                }),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Rename…".into(),
                icon_name: "document-edit-symbolic".into(),
                activate: Box::new(move |tray: &mut Tray| {
                    tokio::spawn(tray::rename(tray.shared.clone(), rename.clone()));
                }),
                ..Default::default()
            }
            .into(),
            CheckmarkItem {
                label: "Trusted (accept without asking)".into(),
                checked: device.trusted,
                activate: Box::new(move |tray: &mut Tray| {
                    tray::toggle_trust(&tray.shared, &trust);
                    tray.snapshot.devices = tray::devices(&tray.shared);
                }),
                ..Default::default()
            }
            .into(),
        ],
        ..Default::default()
    }
    .into()
}

fn request_items(request: &Request) -> Vec<MenuItem<Tray>> {
    let mut items = vec![
        StandardItem {
            label: label(&request.label),
            icon_name: "mail-unread-symbolic".into(),
            enabled: false,
            ..Default::default()
        }
        .into(),
        choice(request, "Accept", ACCEPT),
        choice(request, "Decline", REJECT),
    ];
    if request.verified {
        items.push(choice(request, "Always accept from this device", ALWAYS));
    }
    items
}

fn choice(request: &Request, text: &str, choice: &'static str) -> MenuItem<Tray> {
    let id = request.id.clone();
    StandardItem {
        label: text.into(),
        activate: Box::new(move |tray: &mut Tray| {
            let shared = tray.shared.clone();
            let id = id.clone();
            tokio::spawn(async move { shared.decide(&id, choice).await });
        }),
        ..Default::default()
    }
    .into()
}

fn transfer_items(transfer: &Transfer) -> Vec<MenuItem<Tray>> {
    let id = transfer.id.clone();
    vec![
        StandardItem {
            label: label(&transfer.label),
            enabled: false,
            ..Default::default()
        }
        .into(),
        StandardItem {
            label: "Cancel".into(),
            icon_name: "process-stop-symbolic".into(),
            activate: Box::new(move |tray: &mut Tray| {
                let shared = tray.shared.clone();
                let id = id.clone();
                tokio::spawn(async move { shared.cancel(&id).await });
            }),
            ..Default::default()
        }
        .into(),
    ]
}

struct Tray {
    shared: Arc<Shared>,
    snapshot: Snapshot,
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
        if self.snapshot.requests.is_empty() {
            Status::Active
        } else {
            Status::NeedsAttention
        }
    }

    /// Picks up names and trust changed from the CLI; new devices appear in
    /// the open menu as they answer.
    fn menu_about_to_show(&mut self) {
        self.snapshot.devices = tray::devices(&self.shared);
        self.shared.announce();
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let mut items = Vec::new();
        for request in &self.snapshot.requests {
            items.extend(request_items(request));
            items.push(MenuItem::Separator);
        }
        for transfer in &self.snapshot.transfers {
            items.extend(transfer_items(transfer));
            items.push(MenuItem::Separator);
        }
        if self.snapshot.devices.is_empty() {
            items.push(
                StandardItem {
                    label: "No device found".into(),
                    enabled: false,
                    ..Default::default()
                }
                .into(),
            );
        } else {
            items.extend(self.snapshot.devices.iter().map(device_item));
        }
        items.push(MenuItem::Separator);
        items.push(
            StandardItem {
                label: "Open received files".into(),
                icon_name: "folder-download-symbolic".into(),
                activate: Box::new(|tray: &mut Self| {
                    tokio::spawn(tray::open_received(tray.shared.clone()));
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

/// Shows the tray icon and keeps its menu up to date.
pub async fn run(shared: Arc<Shared>) {
    let tray = Tray {
        shared: shared.clone(),
        snapshot: Snapshot::default(),
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
    tray::watch(&shared, |snapshot| {
        let handle = handle.clone();
        async move {
            handle
                .update(|tray: &mut Tray| tray.snapshot = snapshot)
                .await
                .is_some()
        }
    })
    .await;
}
