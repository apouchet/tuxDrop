//! The control API used by the CLI and the file manager, whatever carries it
//! (session bus or local socket, see [`crate::platform`]).

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tuxdrop_core::client::{self, SendEvent, SendOutcome, SendRequest};
use tuxdrop_core::ipc::{PeerEntry, PendingEntry};
use tuxdrop_core::{Node, random_id};

use crate::{Shared, platform, tray};

/// How long [`Shared::share`] waits for a device to answer when none is known.
const SHARE_SEARCH: Duration = Duration::from_secs(4);

pub enum ApiError {
    InvalidArgs(String),
    Failed(String),
}

impl Shared {
    /// Devices seen in the last 5 minutes.
    pub fn list_peers(&self) -> Vec<PeerEntry> {
        let names = Node::name_store().load();
        self.live_peers()
            .into_iter()
            .map(|(p, seen)| PeerEntry {
                name: names.get(&p.info.fingerprint).unwrap_or("").to_owned(),
                addr: SocketAddr::new(p.addr, p.port).to_string(),
                age: seen.elapsed().as_secs() as u32,
                protocol: p.protocol.scheme().to_owned(),
                alias: p.info.alias,
                fingerprint: p.info.fingerprint,
            })
            .collect()
    }

    pub fn list_pending(&self) -> Vec<PendingEntry> {
        self.pending
            .lock()
            .unwrap()
            .iter()
            .map(|(id, p)| {
                let r = &p.request;
                PendingEntry {
                    id: id.clone(),
                    alias: r.sender.alias.clone(),
                    verified: r.is_verified(),
                    files: r.files.len() as u32,
                    size: r.total_size,
                }
            })
            .collect()
    }

    /// Accepts or declines a pending request (`ACCEPT`, `REJECT`, `ALWAYS`).
    pub async fn choose(&self, id: &str, choice: &str) -> Result<(), ApiError> {
        if self.decide(id, choice).await {
            Ok(())
        } else {
            Err(ApiError::InvalidArgs(format!("no pending transfer {id}")))
        }
    }

    /// Cancels a transfer in progress, incoming (request id) or outgoing
    /// (id returned by [`Self::send`]).
    pub async fn cancel_transfer(&self, id: &str) -> Result<(), ApiError> {
        if self.cancel(id).await {
            Ok(())
        } else {
            Err(ApiError::InvalidArgs(format!(
                "no transfer {id} in progress"
            )))
        }
    }

    /// Starts sending in the background; returns the transfer id used by
    /// the progress and end signals.
    pub async fn send(
        self: &Arc<Self>,
        target: &str,
        paths: Vec<PathBuf>,
    ) -> Result<String, ApiError> {
        let files = match client::collect_files(&paths) {
            Ok(files) => files,
            Err(e) => return Err(self.send_failed(ApiError::InvalidArgs, e).await),
        };
        let peer = match self.find_peer(target).await {
            Ok(peer) => peer,
            Err(e) => return Err(self.send_failed(ApiError::Failed, e).await),
        };
        let id = random_id();
        let shared = self.clone();
        let transfer_id = id.clone();
        tokio::spawn(async move {
            let alias = peer.info.alias.clone();
            let count = format!("{} file(s)", files.len());
            let cancel = CancellationToken::new();
            shared
                .start_transfer(
                    &transfer_id,
                    0,
                    format!("Sending to {alias}"),
                    &format!("{count}, waiting for {alias} to accept"),
                    files.iter().map(|f| f.meta.size).sum(),
                    Some(cancel.clone()),
                )
                .await;
            let (tx, mut rx) = mpsc::unbounded_channel();
            let progress: client::ProgressFn = Arc::new(move |e| {
                let _ = tx.send(e);
            });
            let req = SendRequest {
                identity: &shared.node.identity,
                me: &shared.node.me,
                peer: &peer,
                files,
                pin: None,
            };
            let transfer = client::send(req, progress, cancel.clone());
            tokio::pin!(transfer);
            let mut last_percent = u64::MAX;
            let result = loop {
                tokio::select! {
                    r = &mut transfer => break r,
                    // Matching `Progress` in the pattern would disable this
                    // branch at the first other event until the end.
                    Some(event) = rx.recv() => {
                        let SendEvent::Progress { file_name, sent, total } = event else {
                            continue;
                        };
                        let percent = (sent * 100).checked_div(total).unwrap_or(100);
                        if percent != last_percent {
                            last_percent = percent;
                            shared.signals.progress(&transfer_id, &file_name, sent, total).await;
                        }
                        shared.transfer_progress(&transfer_id, &file_name, sent).await;
                    }
                }
            };
            let notification = shared
                .end_transfer(&transfer_id)
                .map_or(0, |t| t.notification);
            let (summary, body, outcome) = match result {
                Ok(SendOutcome::Completed | SendOutcome::NothingToSend) => {
                    (format!("Sent to {alias}"), count, "completed".to_owned())
                }
                Err(_) if cancel.is_cancelled() => (
                    format!("Sending to {alias} cancelled"),
                    count,
                    "cancelled".to_owned(),
                ),
                Err(e) => (
                    format!("Sending to {alias} failed"),
                    format!("{e:#}"),
                    format!("failed: {e:#}"),
                ),
            };
            shared.conclude(notification, &summary, &body, None).await;
            shared
                .signals
                .transfer_finished(&transfer_id, &outcome)
                .await;
        });
        Ok(id)
    }

    /// Asks the user which nearby device to send `paths` to, then sends
    /// them; returns once the paths are checked. Used from the file
    /// manager when it cannot list the devices itself (Finder).
    pub fn share(self: &Arc<Self>, paths: Vec<PathBuf>) -> Result<(), ApiError> {
        if paths.is_empty() {
            return Err(ApiError::InvalidArgs("nothing to send".to_owned()));
        }
        if let Some(missing) = paths.iter().find(|p| !p.is_absolute() || !p.exists()) {
            return Err(ApiError::InvalidArgs(format!(
                "{}: not an existing absolute path",
                missing.display()
            )));
        }
        tokio::spawn(self.clone().pick_and_send(paths));
        Ok(())
    }

    async fn pick_and_send(self: Arc<Self>, paths: Vec<PathBuf>) {
        self.announce();
        let deadline = Instant::now() + SHARE_SEARCH;
        let mut devices = tray::devices(&self);
        while devices.is_empty() && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(300)).await;
            devices = tray::devices(&self);
        }
        if devices.is_empty() {
            self.notifier
                .inform(
                    0,
                    "No device found",
                    "Open LocalSend or tuxDrop on the other device, on the same network",
                )
                .await;
            return;
        }
        // The dialog returns the label picked: tell homonyms apart.
        let labels: Vec<String> = devices
            .iter()
            .map(|d| {
                if devices.iter().filter(|o| o.label == d.label).count() > 1 {
                    format!(
                        "{} ({})",
                        d.label,
                        &d.fingerprint[..d.fingerprint.len().min(8)]
                    )
                } else {
                    d.label.clone()
                }
            })
            .collect();
        let what = match paths.as_slice() {
            [one] => one.file_name().map_or_else(
                || one.display().to_string(),
                |n| format!("“{}”", n.to_string_lossy()),
            ),
            many => format!("{} items", many.len()),
        };
        match platform::pick_device(&format!("Send {what} to:"), &labels).await {
            Ok(Some(i)) => {
                let _ = self.send(&devices[i].fingerprint, paths).await;
            }
            Ok(None) => {} // cancelled
            Err(e) => {
                tracing::warn!("cannot ask for a device: {e:#}");
                self.notifier
                    .inform(0, "Cannot choose a device", platform::DIALOG_HELP)
                    .await;
            }
        }
    }

    /// Also shows `e` as a notification: callers such as the file manager
    /// menu have no other way to report it.
    async fn send_failed(&self, kind: fn(String) -> ApiError, e: anyhow::Error) -> ApiError {
        let message = format!("{e:#}");
        self.notifier.inform(0, "Sending failed", &message).await;
        kind(message)
    }
}
