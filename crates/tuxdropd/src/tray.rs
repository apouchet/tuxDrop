//! System tray icon (StatusNotifierItem) listing nearby devices.
//!
//! GNOME shows it only with the AppIndicator extension; without a tray host
//! the icon waits for one and the daemon works as before.

use std::sync::Arc;
use std::time::Duration;

use ksni::menu::StandardItem;
use ksni::{MenuItem, OfflineReason, TrayMethods};

use crate::Shared;

/// How often expired devices are dropped from the menu when no new one shows up.
const EXPIRY_CHECK: Duration = Duration::from_secs(30);

struct Tray {
    shared: Arc<Shared>,
    /// Menu labels of the nearby devices, sorted.
    devices: Vec<String>,
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

    /// New devices appear in the open menu as they answer.
    fn menu_about_to_show(&mut self) {
        self.shared.announce();
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let mut items: Vec<MenuItem<Self>> = if self.devices.is_empty() {
            vec![
                StandardItem {
                    label: "No device found".into(),
                    enabled: false,
                    ..Default::default()
                }
                .into(),
            ]
        } else {
            self.devices
                .iter()
                .map(|label| {
                    StandardItem {
                        label: label.clone(),
                        ..Default::default()
                    }
                    .into()
                })
                .collect()
        };
        items.push(MenuItem::Separator);
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

/// Menu labels for the devices currently known to the daemon.
fn device_labels(shared: &Shared) -> Vec<String> {
    let mut labels: Vec<String> = shared
        .live_peers()
        .into_iter()
        // A single underscore marks a mnemonic in menu labels.
        .map(|(peer, _)| peer.info.alias.replace('_', "__"))
        .collect();
    labels.sort_by_key(|l| l.to_lowercase());
    labels
}

/// Shows the tray icon and keeps its device list up to date.
pub async fn run(shared: Arc<Shared>) {
    let tray = Tray {
        shared: shared.clone(),
        devices: Vec::new(),
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
    let mut shown = Vec::new();
    loop {
        let labels = device_labels(&shared);
        if labels != shown {
            shown = labels.clone();
            if handle.update(|tray| tray.devices = labels).await.is_none() {
                return;
            }
        }
        tokio::select! {
            _ = shared.peers_changed.notified() => {}
            _ = tokio::time::sleep(EXPIRY_CHECK) => {}
        }
    }
}
